//! Bock Language Server Protocol implementation.
//!
//! Provides an LSP server over stdio that editors can connect to for
//! diagnostics, hover, go-to-definition, find-references, rename,
//! document symbols, inlay hints, and other Bock tooling features.
//!
//! This crate exposes [`BockLanguageServer`] and a [`run_stdio`] entry point
//! used by `bock-cli` to launch the server.

mod diagnostics;
mod document_symbol;
mod goto_definition;
mod hover;
mod inlay_hint;
mod pipeline;
mod references;
mod rename;
mod server;
mod symbol_index;
mod type_display;

pub use diagnostics::{severity_to_lsp, span_to_range, to_lsp_diagnostic};
pub use document_symbol::{document_symbols, to_lsp_symbols, DocumentSymbolsResult, SymbolNode};
pub use goto_definition::{find_definition, position_to_offset, DefinitionResult};
pub use hover::{hover, HoverResult};
pub use inlay_hint::{inlay_hints, InlayHintsResult, TypeHint, TYPE_RENDER_BUDGET};
pub use pipeline::{check_document, CheckResult};
pub use references::{find_occurrences, SymbolOccurrences};
pub use rename::{validate_new_name, RenameError};
pub use server::BockLanguageServer;
pub use type_display::format_type;

use tower_lsp::{LspService, Server};

/// Launch the Bock LSP server over stdio.
///
/// Blocks the current thread until the client disconnects. Intended to be
/// invoked from an async runtime by `bock lsp`.
pub async fn run_stdio() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(BockLanguageServer::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
