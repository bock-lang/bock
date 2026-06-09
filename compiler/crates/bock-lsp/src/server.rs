//! [`BockLanguageServer`] — stdio LSP server implementing the
//! [`tower_lsp::LanguageServer`] trait.
//!
//! F.1.2 adds live diagnostics: `did_open`/`did_change` run the full check
//! pipeline on the edited document and publish the resulting diagnostics
//! back to the client. `did_close` clears them. The navigation trio —
//! find-references, rename (with prepare), and document symbols — reuses
//! the same single-file pipeline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use tower_lsp::jsonrpc::{Error as LspError, Result as LspResult};
use tower_lsp::lsp_types::{
    DiagnosticOptions, DiagnosticServerCapabilities, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DocumentSymbolParams,
    DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents,
    HoverParams, HoverProviderCapability, InitializeParams, InitializeResult, InitializedParams,
    Location, MarkupContent, MarkupKind, MessageType, OneOf, PrepareRenameResponse,
    ReferenceParams, RenameOptions, RenameParams, ServerCapabilities, ServerInfo,
    TextDocumentPositionParams, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url,
    WorkDoneProgressOptions, WorkspaceEdit,
};
use tower_lsp::{Client, LanguageServer};

use crate::diagnostics::{span_to_range, to_lsp_diagnostic};
use crate::document_symbol::{document_symbols, to_lsp_symbols};
use crate::goto_definition::find_definition;
use crate::hover::hover;
use crate::pipeline::check_document;
use crate::references::find_occurrences;
use crate::rename::validate_new_name;

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
            references_provider: Some(OneOf::Left(true)),
            rename_provider: Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })),
            document_symbol_provider: Some(OneOf::Left(true)),
            diagnostic_provider: Some(DiagnosticServerCapabilities::Options(DiagnosticOptions {
                identifier: Some("bock".to_string()),
                inter_file_dependencies: true,
                workspace_diagnostics: false,
                work_done_progress_options: WorkDoneProgressOptions::default(),
            })),
            ..ServerCapabilities::default()
        }
    }

    /// Fetch the current contents of `uri` from the document store.
    fn document_text(&self, uri: &Url) -> Option<String> {
        self.documents.get(uri).map(|e| e.value().clone())
    }

    /// Log a panic that escaped a blocking navigation task.
    async fn log_task_panic(&self, what: &str, err: impl std::fmt::Display) {
        self.client
            .log_message(MessageType::ERROR, format!("{what} panicked: {err}"))
            .await;
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
        let result = tokio::task::spawn_blocking(move || check_document(path, content)).await;

        let result = match result {
            Ok(r) => r,
            Err(err) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("check pipeline panicked: {err}"),
                    )
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

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;

        let Some(content) = self.document_text(&uri) else {
            return Ok(None);
        };

        let path = url_to_path(&uri);
        // Pipeline is CPU-bound — hop to a blocking thread so we don't
        // stall the LSP reactor.
        let result = tokio::task::spawn_blocking(move || {
            find_occurrences(path, content, pos.line, pos.character)
        })
        .await;

        let result = match result {
            Ok(r) => r,
            Err(err) => {
                self.log_task_panic("references", err).await;
                return Ok(None);
            }
        };

        let Some(occ) = result else { return Ok(None) };

        let source_file = occ.source_map.get_file(occ.file_id);
        let mut spans = occ.reference_spans.clone();
        if include_declaration {
            spans.push(occ.decl_span);
        }
        spans.sort_unstable_by_key(|s| (s.start, s.end));

        let locations = spans
            .into_iter()
            .map(|span| Location {
                uri: uri.clone(),
                range: span_to_range(span, source_file),
            })
            .collect();
        Ok(Some(locations))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> LspResult<Option<PrepareRenameResponse>> {
        let uri = params.text_document.uri;
        let pos = params.position;

        let Some(content) = self.document_text(&uri) else {
            return Ok(None);
        };

        let path = url_to_path(&uri);
        let result = tokio::task::spawn_blocking(move || {
            find_occurrences(path, content, pos.line, pos.character)
        })
        .await;

        let result = match result {
            Ok(r) => r,
            Err(err) => {
                self.log_task_panic("prepare_rename", err).await;
                return Ok(None);
            }
        };

        // `None` (cursor not on a renameable symbol) tells the client to
        // refuse the rename UI for this position.
        Ok(result.map(|occ| {
            let source_file = occ.source_map.get_file(occ.file_id);
            PrepareRenameResponse::RangeWithPlaceholder {
                range: span_to_range(occ.origin_span, source_file),
                placeholder: occ.name.clone(),
            }
        }))
    }

    async fn rename(&self, params: RenameParams) -> LspResult<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = params.new_name;

        let Some(content) = self.document_text(&uri) else {
            return Ok(None);
        };

        let path = url_to_path(&uri);
        let result = tokio::task::spawn_blocking(move || {
            find_occurrences(path, content, pos.line, pos.character)
        })
        .await;

        let result = match result {
            Ok(r) => r,
            Err(err) => {
                self.log_task_panic("rename", err).await;
                return Ok(None);
            }
        };

        let Some(occ) = result else { return Ok(None) };

        if let Err(reason) = validate_new_name(&occ.name, &new_name) {
            return Err(LspError::invalid_params(reason.to_string()));
        }

        let source_file = occ.source_map.get_file(occ.file_id);
        let edits: Vec<TextEdit> = occ
            .reference_spans
            .iter()
            .chain(std::iter::once(&occ.decl_span))
            .map(|span| TextEdit {
                range: span_to_range(*span, source_file),
                new_text: new_name.clone(),
            })
            .collect();

        let mut changes = HashMap::new();
        changes.insert(uri, edits);
        Ok(Some(WorkspaceEdit {
            changes: Some(changes),
            ..WorkspaceEdit::default()
        }))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> LspResult<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;

        let Some(content) = self.document_text(&uri) else {
            return Ok(None);
        };

        let path = url_to_path(&uri);
        let result = tokio::task::spawn_blocking(move || document_symbols(path, content)).await;

        let result = match result {
            Ok(r) => r,
            Err(err) => {
                self.log_task_panic("document_symbol", err).await;
                return Ok(None);
            }
        };

        let source_file = result.source_map.get_file(result.file_id);
        let symbols = to_lsp_symbols(&result.symbols, source_file);
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
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
            matches!(
                caps.hover_provider,
                Some(HoverProviderCapability::Simple(true))
            ),
            "hover provider must be enabled",
        );

        assert!(
            matches!(caps.definition_provider, Some(OneOf::Left(true))),
            "definition provider must be enabled",
        );

        assert!(
            matches!(caps.references_provider, Some(OneOf::Left(true))),
            "references provider must be enabled",
        );

        match caps.rename_provider {
            Some(OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                ..
            })) => {}
            other => panic!("rename provider with prepare support expected, got {other:?}"),
        }

        assert!(
            matches!(caps.document_symbol_provider, Some(OneOf::Left(true))),
            "document symbol provider must be enabled",
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
