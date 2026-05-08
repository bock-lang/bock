//! Code generator trait and output types.

use std::path::PathBuf;

use bock_air::NodeKind;
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::profile::TargetProfile;

// ─── GeneratedCode ───────────────────────────────────────────────────────────

/// Output from code generation — consistent across all targets.
#[derive(Debug, Clone)]
pub struct GeneratedCode {
    /// Generated output files (path + content pairs).
    pub files: Vec<OutputFile>,
    /// Source map from AIR spans to target spans (optional for v1).
    pub source_map: Option<SourceMap>,
}

/// A single generated output file.
#[derive(Debug, Clone)]
pub struct OutputFile {
    /// Relative path for the output file.
    pub path: PathBuf,
    /// Generated source code content.
    pub content: String,
}

/// Maps AIR source spans to generated code spans.
///
/// Populated by JS/TS code generators with pointwise mappings from generated
/// `(line, col)` back to source `(line, col)`. For other targets, only the
/// legacy `entries` list (AIR node id → target byte range) is populated.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    /// Legacy entries keyed by AIR node id (present for all targets).
    pub entries: Vec<SourceMapEntry>,
    /// Pointwise position mappings from generated code to source.
    pub mappings: Vec<SourceMapping>,
    /// The generated output file these mappings refer to (e.g. `"output.js"`).
    pub generated_file: String,
    /// Source files referenced by `mappings`, in file-id order.
    /// Each entry is `(path, optional_inline_content)`.
    pub sources: Vec<SourceInfo>,
}

/// A single source-map entry linking an AIR span to a target span.
#[derive(Debug, Clone)]
pub struct SourceMapEntry {
    /// AIR node id.
    pub air_node_id: u32,
    /// Index into `GeneratedCode::files`.
    pub file_index: usize,
    /// Byte offset in the generated file.
    pub target_start: usize,
    /// Byte length in the generated file.
    pub target_len: usize,
}

/// A single pointwise mapping from a position in generated code to a position
/// in the originating Bock source.
#[derive(Debug, Clone)]
pub struct SourceMapping {
    /// 1-indexed line in the generated file.
    pub gen_line: u32,
    /// 1-indexed column (character count) in the generated file.
    pub gen_col: u32,
    /// 1-indexed source line. `0` means unresolved — call
    /// [`SourceMap::resolve_positions`] with source content to fill this in.
    pub src_line: u32,
    /// 1-indexed source column. `0` when unresolved.
    pub src_col: u32,
    /// Byte offset into the source file; used to (re)compute line/col.
    pub src_offset: u32,
    /// File-registry id of the source file (index into `SourceMap::sources`).
    pub src_file_id: u32,
}

/// Metadata for a source file referenced by a [`SourceMap`].
#[derive(Debug, Clone)]
pub struct SourceInfo {
    /// File path (relative or absolute), as it should appear in the emitted
    /// source-map JSON.
    pub path: String,
    /// Optional inline content — when present, embedded into the `.map` file
    /// via `sourcesContent`.
    pub content: Option<String>,
}

impl SourceMap {
    /// Fills in `src_line` and `src_col` on every mapping by looking up
    /// `src_offset` inside `sources_content`, which is indexed by
    /// `src_file_id`. Mappings whose `src_file_id` is out of range are left
    /// unresolved.
    pub fn resolve_positions(&mut self, sources_content: &[&str]) {
        for m in &mut self.mappings {
            let Some(src) = sources_content.get(m.src_file_id as usize) else {
                continue;
            };
            let (line, col) = byte_to_line_col(src, m.src_offset as usize);
            m.src_line = line;
            m.src_col = col;
        }
    }

    /// Emits a Source Map v3 JSON document referring to this map's
    /// `generated_file` and `sources`. Only mappings whose `src_line` is
    /// non-zero are included.
    #[must_use]
    pub fn to_source_map_v3_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\"version\":3,\"file\":\"");
        out.push_str(&escape_json(&self.generated_file));
        out.push_str("\",\"sourceRoot\":\"\",\"sources\":[");
        for (i, s) in self.sources.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(&escape_json(&s.path));
            out.push('"');
        }
        out.push_str("],\"sourcesContent\":[");
        for (i, s) in self.sources.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            match &s.content {
                Some(c) => {
                    out.push('"');
                    out.push_str(&escape_json(c));
                    out.push('"');
                }
                None => out.push_str("null"),
            }
        }
        out.push_str("],\"names\":[],\"mappings\":\"");
        out.push_str(&encode_vlq_mappings(&self.mappings));
        out.push_str("\"}");
        out
    }
}

/// Convert a UTF-8 byte offset into a 1-indexed (line, column) pair. Column
/// counts Unicode scalar values, not bytes — matching `bock-source`.
fn byte_to_line_col(src: &str, offset: usize) -> (u32, u32) {
    let offset = offset.min(src.len());
    let before = &src[..offset];
    let line = before.bytes().filter(|b| *b == b'\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let col = src[line_start..offset].chars().count() as u32 + 1;
    (line, col)
}

/// Minimal JSON string escaper for the small subset of characters that
/// appear in paths and source files.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Encode mappings as a Source Map v3 "mappings" string (semicolons between
/// generated lines, commas between segments, VLQ-encoded deltas).
fn encode_vlq_mappings(mappings: &[SourceMapping]) -> String {
    let mut resolved: Vec<&SourceMapping> = mappings.iter().filter(|m| m.src_line > 0).collect();
    resolved.sort_by_key(|m| (m.gen_line, m.gen_col));

    let mut out = String::new();
    let mut prev_gen_line: u32 = 1;
    let mut prev_gen_col: i64 = 0;
    let mut prev_src_file: i64 = 0;
    let mut prev_src_line: i64 = 0;
    let mut prev_src_col: i64 = 0;

    let mut first_on_line = true;
    for m in resolved {
        while prev_gen_line < m.gen_line {
            out.push(';');
            prev_gen_line += 1;
            prev_gen_col = 0;
            first_on_line = true;
        }
        if !first_on_line {
            out.push(',');
        }
        let gen_col = (m.gen_col as i64) - 1;
        let src_file = m.src_file_id as i64;
        let src_line = (m.src_line as i64) - 1;
        let src_col = (m.src_col as i64) - 1;

        vlq_encode(&mut out, gen_col - prev_gen_col);
        vlq_encode(&mut out, src_file - prev_src_file);
        vlq_encode(&mut out, src_line - prev_src_line);
        vlq_encode(&mut out, src_col - prev_src_col);

        prev_gen_col = gen_col;
        prev_src_file = src_file;
        prev_src_line = src_line;
        prev_src_col = src_col;
        first_on_line = false;
    }
    out
}

/// Base-64 VLQ encode a single signed integer onto `out`.
fn vlq_encode(out: &mut String, value: i64) {
    const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut v: u64 = if value < 0 {
        ((-value as u64) << 1) | 1
    } else {
        (value as u64) << 1
    };
    loop {
        let mut digit = (v & 0x1F) as u8;
        v >>= 5;
        if v != 0 {
            digit |= 0x20;
        }
        out.push(BASE64[digit as usize] as char);
        if v == 0 {
            break;
        }
    }
}

// ─── CodeGenerator trait ─────────────────────────────────────────────────────

/// The trait all per-target code generators implement.
///
/// Each target (JS, TS, Python, Rust, Go) provides a struct that implements
/// this trait. The `generate_module` method transforms a fully-lowered AIR
/// module into target-specific source code.
pub trait CodeGenerator {
    /// Returns the target profile for this generator.
    fn target(&self) -> &TargetProfile;

    /// Returns `true` when the given AIR node should go through Tier 1
    /// AI synthesis (§17.2, Q3 amended).
    ///
    /// The default implementation consults [`TargetProfile::ai_hints`]
    /// via [`crate::ai_synthesis::needs_ai_synthesis`]. Backends that
    /// want per-node overrides (e.g., only non-trivial `match`
    /// expressions) can override this method.
    fn needs_ai_synthesis(&self, node: &bock_air::AIRNode) -> bool {
        crate::ai_synthesis::needs_ai_synthesis(self.target(), node)
    }

    /// Generates target code from a fully-lowered AIR module.
    ///
    /// # Errors
    ///
    /// Returns `CodegenError` if the module contains constructs that cannot
    /// be represented in the target language.
    fn generate_module(&self, module: &AIRModule) -> Result<GeneratedCode, CodegenError>;

    /// Returns the source-code snippet that invokes the user's `main` function
    /// as the entry point for this target, or `None` if the target has a
    /// native entry-point convention (Rust `fn main`, Go `func main`) that
    /// runs without a synthetic call.
    ///
    /// `main_is_async` is `true` when the user's `main` function is declared
    /// `async fn`; targets with native async runtimes (JS, TS, Python) wrap
    /// the call in an event-loop driver in that case.
    ///
    /// Targets that need a trailing invocation (JS, TS, Python) override this
    /// to return e.g. `"main();\n"`. The default `generate_project` appends
    /// the snippet when any module declares a top-level `main` function.
    fn entry_invocation(&self, main_is_async: bool) -> Option<String> {
        let _ = main_is_async;
        None
    }

    /// Generates target code from multiple AIR modules, producing a single concatenated output.
    ///
    /// The default implementation generates each module separately and joins the
    /// results into one file. Generators that need to deduplicate preambles
    /// (e.g., Go's `package` declaration) should override this method.
    fn generate_project(&self, modules: &[&AIRModule]) -> Result<GeneratedCode, CodegenError> {
        let mut combined = String::new();
        let mut merged_map: Option<SourceMap> = None;
        for module in modules {
            let code = self.generate_module(module)?;
            // For each file, record where its content starts in `combined`
            // (as a 0-indexed line count). Generators typically emit one file.
            let mut file_shifts: Vec<u32> = Vec::with_capacity(code.files.len());
            for file in &code.files {
                if !combined.is_empty() && !file.content.is_empty() {
                    combined.push('\n');
                }
                file_shifts.push(count_newlines(&combined) as u32);
                combined.push_str(&file.content);
            }
            if let Some(mut sm) = code.source_map {
                let shift = file_shifts.first().copied().unwrap_or(0);
                for m in &mut sm.mappings {
                    m.gen_line = m.gen_line.saturating_add(shift);
                }
                match &mut merged_map {
                    Some(acc) => {
                        acc.mappings.append(&mut sm.mappings);
                        for src in sm.sources {
                            if !acc.sources.iter().any(|s| s.path == src.path) {
                                acc.sources.push(src);
                            }
                        }
                    }
                    None => merged_map = Some(sm),
                }
            }
        }

        let main_is_async = modules.iter().any(|m| module_main_fn_is_async(m));
        if let Some(invocation) = self.entry_invocation(main_is_async) {
            if modules.iter().any(|m| module_declares_main_fn(m)) {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&invocation);
            }
        }

        let ext = &self.target().conventions.file_extension;
        let out_path = format!("output.{ext}");
        if let Some(sm) = &mut merged_map {
            sm.generated_file = out_path.clone();
        }
        Ok(GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::from(out_path),
                content: combined,
            }],
            source_map: merged_map,
        })
    }
}

fn count_newlines(s: &str) -> usize {
    s.bytes().filter(|b| *b == b'\n').count()
}

/// Returns true if the given AIR module declares a top-level function named
/// `main`. Used by the build pipeline to decide whether to append an
/// entry-point invocation to the generated output of targets without a
/// native main convention.
#[must_use]
pub fn module_declares_main_fn(module: &AIRModule) -> bool {
    let NodeKind::Module { items, .. } = &module.kind else {
        return false;
    };
    items.iter().any(|item| {
        matches!(
            &item.kind,
            NodeKind::FnDecl { name, .. } if name.name == "main"
        )
    })
}

/// Returns true if the given AIR module declares a top-level `async fn main`.
/// Used by `generate_project` to select an async-aware entry invocation.
#[must_use]
pub fn module_main_fn_is_async(module: &AIRModule) -> bool {
    let NodeKind::Module { items, .. } = &module.kind else {
        return false;
    };
    items.iter().any(|item| {
        matches!(
            &item.kind,
            NodeKind::FnDecl { name, is_async: true, .. } if name.name == "main"
        )
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_file_stores_path_and_content() {
        let f = OutputFile {
            path: PathBuf::from("main.js"),
            content: "console.log('hello');".into(),
        };
        assert_eq!(f.path, PathBuf::from("main.js"));
        assert!(f.content.contains("console.log"));
    }

    #[test]
    fn generated_code_with_no_source_map() {
        let code = GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::from("out.py"),
                content: "print('hello')".into(),
            }],
            source_map: None,
        };
        assert_eq!(code.files.len(), 1);
        assert!(code.source_map.is_none());
    }

    #[test]
    fn source_map_default_is_empty() {
        let sm = SourceMap::default();
        assert!(sm.entries.is_empty());
        assert!(sm.mappings.is_empty());
        assert!(sm.sources.is_empty());
    }

    #[test]
    fn byte_to_line_col_basic() {
        let s = "abc\ndef\nghi";
        assert_eq!(byte_to_line_col(s, 0), (1, 1));
        assert_eq!(byte_to_line_col(s, 3), (1, 4));
        assert_eq!(byte_to_line_col(s, 4), (2, 1));
        assert_eq!(byte_to_line_col(s, 8), (3, 1));
    }

    #[test]
    fn resolve_positions_fills_line_col() {
        let mut sm = SourceMap {
            mappings: vec![SourceMapping {
                gen_line: 1,
                gen_col: 1,
                src_line: 0,
                src_col: 0,
                src_offset: 4,
                src_file_id: 0,
            }],
            ..Default::default()
        };
        sm.resolve_positions(&["abc\ndef"]);
        assert_eq!(sm.mappings[0].src_line, 2);
        assert_eq!(sm.mappings[0].src_col, 1);
    }

    #[test]
    fn vlq_encodes_known_values() {
        // Source Map v3 VLQ reference values.
        let mut s = String::new();
        vlq_encode(&mut s, 0);
        assert_eq!(s, "A");
        s.clear();
        vlq_encode(&mut s, 1);
        assert_eq!(s, "C");
        s.clear();
        vlq_encode(&mut s, -1);
        assert_eq!(s, "D");
        s.clear();
        vlq_encode(&mut s, 16);
        assert_eq!(s, "gB");
    }

    #[test]
    fn source_map_v3_json_contains_required_fields() {
        let mut sm = SourceMap {
            generated_file: "output.js".into(),
            ..Default::default()
        };
        sm.sources.push(SourceInfo {
            path: "main.bock".into(),
            content: Some("let x = 1\n".into()),
        });
        sm.mappings.push(SourceMapping {
            gen_line: 1,
            gen_col: 1,
            src_line: 1,
            src_col: 1,
            src_offset: 0,
            src_file_id: 0,
        });
        let json = sm.to_source_map_v3_json();
        assert!(json.contains("\"version\":3"));
        assert!(json.contains("\"file\":\"output.js\""));
        assert!(json.contains("\"sources\":[\"main.bock\"]"));
        assert!(json.contains("\"mappings\":"));
    }

    // ── module_declares_main_fn ─────────────────────────────────────────────

    use bock_air::AIRNode;
    use bock_ast::{Ident, Visibility};
    use bock_errors::{FileId, Span};

    fn dummy_span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: dummy_span(),
        }
    }

    fn fn_decl(name: &str) -> AIRNode {
        let body = AIRNode::new(
            1,
            dummy_span(),
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        AIRNode::new(
            0,
            dummy_span(),
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        )
    }

    fn module_with(items: Vec<AIRNode>) -> AIRNode {
        AIRNode::new(
            0,
            dummy_span(),
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items,
            },
        )
    }

    #[test]
    fn module_declares_main_detects_top_level_main() {
        let m = module_with(vec![fn_decl("helper"), fn_decl("main")]);
        assert!(module_declares_main_fn(&m));
    }

    #[test]
    fn module_declares_main_returns_false_when_absent() {
        let m = module_with(vec![fn_decl("helper"), fn_decl("other")]);
        assert!(!module_declares_main_fn(&m));
    }

    #[test]
    fn module_declares_main_returns_false_for_empty_module() {
        let m = module_with(vec![]);
        assert!(!module_declares_main_fn(&m));
    }
}
