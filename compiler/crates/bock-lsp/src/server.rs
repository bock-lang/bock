//! [`BockLanguageServer`] — stdio LSP server implementing the
//! [`tower_lsp::LanguageServer`] trait.
//!
//! F.1.2 adds live diagnostics: `did_open`/`did_change` run the full check
//! pipeline on the edited document and publish the resulting diagnostics
//! back to the client. `did_close` clears them.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    DiagnosticOptions, DiagnosticServerCapabilities, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, InitializedParams, Location, MarkupContent, MarkupKind,
    MessageType, OneOf, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url, WorkDoneProgressOptions,
};
use tower_lsp::{Client, LanguageServer};

use crate::diagnostics::{span_to_range, to_lsp_diagnostic};
use crate::goto_definition::find_definition;
use crate::hover::hover;
use crate::pipeline::check_document;

/// The Bock language server.
///
/// Holds a [`Client`] handle for publishing diagnostics and logging, plus a
/// concurrent map of open documents keyed by their URI.
#[derive(Debug)]
pub struct BockLanguageServer {
    client: Client,
    documents: Arc<DashMap<Url, String>>,
}

impl BockLanguageServer {
    /// Construct a new server bound to the given LSP client.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(DashMap::new()),
        }
    }

    fn server_capabilities() -> ServerCapabilities {
        ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            definition_provider: Some(OneOf::Left(true)),
            diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("bock".to_string()),
                inter_file_dependencies: true,
                workspace_diagnostics: false,
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })),
            ..ServerCapabilities::default()
        }
    }

    /// Re-run the check pipeline on `uri`'s current contents and publish the
    /// resulting diagnostics. No-op if the URI is not in the document store.
    async fn publish(&self, uri: Url, version: Option<i32>) {
        let Some(content) = self.documents.get(&uri).map(|e| e.value().clone()) else {
            return;
        };

        let path = url_to_path(&uri);
        let uri_for_task = uri.clone();
        // The pipeline is CPU-bound — hop to a blocking thread so we don't
        // stall the LSP reactor when checking a large file.
        let result =
            tokio::task::spawn_blocking(move || check_document(path, content)).await;

        let result = match result {
            Ok(r) => r,
            Err(err) => {
                self.client
                    .log_message(MessageType::ERROR, format!("check pipeline panicked: {err}"))
                    .await;
                return;
            }
        };

        let source_file = result.source_map.get_file(result.file_id);
        let lsp_diags: Vec<_> = result
            .diagnostics
            .iter()
            .map(|d| to_lsp_diagnostic(d, &uri_for_task, source_file))
            .collect();

        self.client
            .publish_diagnostics(uri_for_task, lsp_diags, version)
            .await;
    }
}

/// Best-effort conversion of a `file://` URI to a [`PathBuf`]. Non-file URIs
/// fall back to the raw path component so diagnostics still render — a
/// synthetic filename is harmless because the LSP never reads from disk.
fn url_to_path(uri: &Url) -> PathBuf {
    uri.to_file_path()
        .unwrap_or_else(|_| PathBuf::from(uri.path()))
}

#[tower_lsp::async_trait]
impl LanguageServer for BockLanguageServer {
    async fn initialize(&self, _params: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: Self::server_capabilities(),
            server_info: Some(ServerInfo {
                name: "bock-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Bock LSP ready")
            .await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        self.documents.insert(doc.uri.clone(), doc.text);
        self.publish(doc.uri, Some(doc.version)).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // We advertise `TextDocumentSyncKind::FULL`, so each change event
        // carries the complete new document text in `text`. Apply the last
        // event (the one the client considers authoritative).
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents.insert(uri.clone(), change.text);
        }
        self.publish(uri, Some(params.text_document.version)).await;
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let Some(content) = self.documents.get(&uri).map(|e| e.value().clone()) else {
            return Ok(None);
        };

        let path = url_to_path(&uri);
        // Pipeline is CPU-bound — hop to a blocking thread so we don't
        // stall the LSP reactor.
        let result = tokio::task::spawn_blocking(move || {
            find_definition(path, content, pos.line, pos.character)
        })
        .await;

        let result = match result {
            Ok(r) => r,
            Err(err) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("goto_definition panicked: {err}"),
                    )
                    .await;
                return Ok(None);
            }
        };

        let Some(def) = result else { return Ok(None) };

        let source_file = def.source_map.get_file(def.file_id);
        let range = span_to_range(def.target, source_file);
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri,
            range,
        })))
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let Some(content) = self.documents.get(&uri).map(|e| e.value().clone()) else {
            return Ok(None);
        };

        let path = url_to_path(&uri);
        // Pipeline is CPU-bound — hop to a blocking thread so we don't
        // stall the LSP reactor.
        let result =
            tokio::task::spawn_blocking(move || hover(path, content, pos.line, pos.character))
                .await;

        let result = match result {
            Ok(r) => r,
            Err(err) => {
                self.client
                    .log_message(MessageType::ERROR, format!("hover panicked: {err}"))
                    .await;
                return Ok(None);
            }
        };

        let Some(info) = result else { return Ok(None) };

        let source_file = info.source_map.get_file(info.file_id);
        let range = span_to_range(info.span, source_file);
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: info.contents,
            }),
            range: Some(range),
        }))
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.remove(&uri);
        // Clear any diagnostics we previously published for this file.
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_declare_required_providers() {
        let caps = BockLanguageServer::server_capabilities();

        match caps.text_document_sync {
            Some(TextDocumentSyncCapability::Kind(kind)) => {
                assert_eq!(kind, TextDocumentSyncKind::FULL);
            }
            _ => panic!("expected Full text document sync"),
        }

        assert!(
            matches!(caps.hover_provider, Some(HoverProviderCapability::Simple(true))),
            "hover provider must be enabled",
        );

        assert!(
            matches!(caps.definition_provider, Some(OneOf::Left(true))),
            "definition provider must be enabled",
        );

        assert!(
            caps.diagnostic_provider.is_some(),
            "diagnostic provider must be declared for F.1.2",
        );
    }

    #[test]
    fn url_to_path_handles_file_uri() {
        let url = Url::parse("file:///tmp/foo.bock").unwrap();
        assert_eq!(url_to_path(&url), PathBuf::from("/tmp/foo.bock"));
    }

    #[test]
    fn url_to_path_falls_back_for_non_file_scheme() {
        let url = Url::parse("untitled:Untitled-1").unwrap();
        let path = url_to_path(&url);
        assert!(!path.as_os_str().is_empty());
    }
}
