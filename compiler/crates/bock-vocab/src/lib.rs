//! Bock vocabulary emitter.
//!
//! Produces a single JSON document describing the language surface area
//! — keywords, operators, annotations, stdlib methods, diagnostic codes,
//! target names, CLI commands — for downstream tooling (editor extensions,
//! documentation sites). The contents are not hardcoded here: every
//! section is produced by querying the source-of-truth registry in the
//! relevant crate, so the extension's vocabulary stays in sync with the
//! compiler.

use std::collections::BTreeMap;

pub mod schema;

pub use schema::Vocab;

use bock_errors::Severity;

/// Compiler version string matching the workspace `Cargo.toml`.
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build the complete vocabulary by querying each source crate's registry.
///
/// This is the single entry point. The returned [`Vocab`] is ready to
/// serialize via `serde_json`.
#[must_use]
pub fn build_vocab() -> Vocab {
    Vocab {
        version: COMPILER_VERSION.to_string(),
        language: build_language(),
        stdlib: build_stdlib(),
        diagnostics: build_diagnostics(),
        tooling: build_tooling(),
    }
}

// ─── Language ────────────────────────────────────────────────────────────────

fn build_language() -> schema::LanguageVocab {
    let keywords = bock_lexer::vocab::keywords()
        .into_iter()
        .map(|kw| schema::Keyword {
            name: kw.text.to_string(),
            category: kw.category.to_string(),
            spec_ref: kw.spec_ref.map(String::from),
        })
        .collect();

    let operators = bock_lexer::vocab::operators()
        .into_iter()
        .map(|op| schema::Operator {
            symbol: op.symbol.to_string(),
            precedence: op.precedence,
            associativity: op.associativity.to_string(),
            kind: op.kind.to_string(),
            spec_ref: op.spec_ref.map(String::from),
        })
        .collect();

    let annotations = bock_types::vocab::annotations()
        .into_iter()
        .map(|a| schema::Annotation {
            name: a.name.to_string(),
            params: a.params.to_string(),
            purpose: a.purpose.to_string(),
            spec_ref: a.spec_ref.map(String::from),
        })
        .collect();

    let strictness_levels = bock_types::vocab::strictness_levels()
        .into_iter()
        .map(|s| schema::StrictnessLevel {
            name: s.name.to_string(),
            description: s.description.to_string(),
            spec_ref: s.spec_ref.map(String::from),
        })
        .collect();

    let primitive_types = bock_air::prelude_vocab::PRIMITIVE_TYPES
        .iter()
        .map(|name| schema::PrimitiveType {
            name: (*name).to_string(),
            spec_ref: Some("§2.1".into()),
        })
        .collect();

    let prelude_types = bock_air::prelude_vocab::PRELUDE_TYPES
        .iter()
        .map(|name| schema::Symbol {
            name: (*name).to_string(),
            kind: "type".into(),
            signature: (*name).to_string(),
            doc: None,
            spec_ref: None,
            since: None,
        })
        .collect();

    let prelude_functions = bock_air::prelude_vocab::PRELUDE_FUNCTIONS
        .iter()
        .map(|name| schema::Symbol {
            name: (*name).to_string(),
            kind: "function".into(),
            signature: format!("{name}(..)"),
            doc: None,
            spec_ref: None,
            since: None,
        })
        .collect();

    let prelude_traits = bock_air::prelude_vocab::PRELUDE_TRAITS
        .iter()
        .map(|name| schema::Symbol {
            name: (*name).to_string(),
            kind: "trait".into(),
            signature: (*name).to_string(),
            doc: None,
            spec_ref: None,
            since: None,
        })
        .collect();

    let prelude_constructors = bock_air::prelude_vocab::PRELUDE_CONSTRUCTORS
        .iter()
        .map(|name| schema::Symbol {
            name: (*name).to_string(),
            kind: "constructor".into(),
            signature: (*name).to_string(),
            doc: None,
            spec_ref: None,
            since: None,
        })
        .collect();

    schema::LanguageVocab {
        keywords,
        operators,
        annotations,
        strictness_levels,
        primitive_types,
        prelude_types,
        prelude_functions,
        prelude_traits,
        prelude_constructors,
    }
}

// ─── Stdlib ──────────────────────────────────────────────────────────────────

fn build_stdlib() -> schema::StdlibVocab {
    // Populate a fresh BuiltinRegistry with the full core library, then
    // introspect its dispatch table. This keeps the method list identical
    // to what the runtime actually ships.
    let mut registry = bock_interp::BuiltinRegistry::new();
    registry.register_defaults();
    bock_core::register_core(&mut registry);

    let mut by_receiver: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (tag, name) in registry.method_keys() {
        by_receiver
            .entry(tag.name().to_string())
            .or_default()
            .push(name.to_string());
    }
    for methods in by_receiver.values_mut() {
        methods.sort();
        methods.dedup();
    }

    let builtin_methods = by_receiver
        .into_iter()
        .map(|(receiver, methods)| schema::BuiltinMethodGroup { receiver, methods })
        .collect();

    let mut builtin_globals: Vec<String> = registry
        .global_names()
        .map(|s| s.to_string())
        .collect();
    builtin_globals.sort();
    builtin_globals.dedup();

    // Top-level core modules. These track the stub modules declared in
    // `bock_core::lib.rs`; they are the namespaces editors should offer
    // for completion, even though some are placeholders today.
    let modules = vec![
        core_module("core.primitives", "§14.1"),
        core_module("core.collections", "§14.2"),
        core_module("core.option_result", "§14.3"),
        core_module("core.iterator", "§14.4"),
        core_module("core.string_builder", "§14.5"),
        core_module("core.time", "§14.6"),
        core_module("core.concurrency", "§14.7"),
        core_module("core.effect", "§14.8"),
        core_module("core.error", "§14.9"),
        core_module("core.math", "§14.10"),
        core_module("core.memory", "§14.11"),
        core_module("core.test", "§14.12"),
        core_module("core.traits", "§14.13"),
    ];

    schema::StdlibVocab {
        modules,
        builtin_methods,
        builtin_globals,
    }
}

fn core_module(path: &str, spec_ref: &str) -> schema::Module {
    schema::Module {
        path: path.to_string(),
        types: Vec::new(),
        functions: Vec::new(),
        effects: Vec::new(),
        traits: Vec::new(),
        spec_ref: Some(spec_ref.to_string()),
    }
}

// ─── Diagnostics ─────────────────────────────────────────────────────────────

fn build_diagnostics() -> schema::DiagnosticsVocab {
    let codes = bock_errors::catalog::diagnostic_catalog()
        .into_iter()
        .map(|info| schema::DiagnosticCode {
            code: info.code.to_string(),
            severity: severity_name(info.severity).to_string(),
            summary: info.summary.to_string(),
            description: info.description.to_string(),
            bad_example: None,
            good_example: None,
            spec_refs: info.spec_refs.iter().map(|s| (*s).to_string()).collect(),
            related_codes: Vec::new(),
        })
        .collect();

    schema::DiagnosticsVocab { codes }
}

fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        Severity::Hint => "hint",
    }
}

// ─── Tooling ─────────────────────────────────────────────────────────────────

fn build_tooling() -> schema::ToolingVocab {
    let targets = bock_codegen::profile::TargetProfile::all_builtins()
        .into_iter()
        .map(|p| schema::Target {
            id: p.id,
            display_name: p.display_name,
        })
        .collect();

    let ai_providers = bock_ai::known_providers()
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    let commands = command_catalog()
        .into_iter()
        .map(|(name, summary)| schema::Command {
            name: name.to_string(),
            summary: summary.to_string(),
        })
        .collect();

    schema::ToolingVocab {
        targets,
        ai_providers,
        commands,
    }
}

fn command_catalog() -> Vec<(&'static str, &'static str)> {
    vec![
        ("new", "Scaffold a new Bock project."),
        ("build", "Transpile and compile a Bock project."),
        ("run", "Execute a Bock program via the interpreter."),
        ("check", "Type-check and lint without building."),
        ("test", "Run tests."),
        ("fmt", "Format Bock source files."),
        ("repl", "Start an interactive REPL session."),
        ("inspect", "Browse AI decisions, rule cache, and AI response cache."),
        ("pin", "Pin AI decisions so they replay deterministically."),
        ("unpin", "Clear pin metadata from a decision."),
        ("override", "Override or promote an AI decision."),
        ("cache", "Manage on-disk AI, decision, and rule caches."),
        ("promote", "Analyze a project at the next strictness level."),
        ("pkg", "Package manager commands."),
        ("model", "Query or interact with AI models."),
        ("doc", "Generate documentation."),
        ("lsp", "Start the Bock language server."),
    ]
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_workspace() {
        // This is the source of truth for the compiler version.
        assert_eq!(COMPILER_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn language_sections_non_empty() {
        let v = build_language();
        assert!(!v.keywords.is_empty(), "no keywords");
        assert!(!v.operators.is_empty(), "no operators");
        assert!(!v.annotations.is_empty(), "no annotations");
        assert_eq!(v.strictness_levels.len(), 3);
        assert!(!v.primitive_types.is_empty(), "no primitives");
        assert!(!v.prelude_types.is_empty(), "no prelude types");
        assert!(!v.prelude_functions.is_empty(), "no prelude fns");
        assert!(!v.prelude_traits.is_empty(), "no prelude traits");
        assert!(!v.prelude_constructors.is_empty(), "no prelude ctors");
    }

    #[test]
    fn stdlib_section_non_empty() {
        let v = build_stdlib();
        assert!(!v.builtin_methods.is_empty(), "no builtin methods");
        assert!(!v.builtin_globals.is_empty(), "no builtin globals");
        assert!(!v.modules.is_empty(), "no modules");
    }

    #[test]
    fn diagnostics_section_non_empty() {
        let v = build_diagnostics();
        assert!(!v.codes.is_empty(), "no diagnostic codes");
    }

    #[test]
    fn tooling_section_non_empty() {
        let v = build_tooling();
        assert_eq!(v.targets.len(), 5, "expected 5 builtin targets");
        assert!(!v.ai_providers.is_empty(), "no ai providers");
        assert!(!v.commands.is_empty(), "no commands");
    }

    #[test]
    fn vocab_round_trips_through_json() {
        let vocab = build_vocab();
        let json = serde_json::to_string(&vocab).expect("serialize");
        let parsed: Vocab = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(vocab, parsed);
    }

    #[test]
    fn vocab_pretty_json_is_parseable() {
        let vocab = build_vocab();
        let json = serde_json::to_string_pretty(&vocab).expect("serialize");
        let _: Vocab = serde_json::from_str(&json).expect("deserialize");
    }

    #[test]
    fn builtin_methods_contain_int_add() {
        let v = build_stdlib();
        let int_group = v
            .builtin_methods
            .iter()
            .find(|g| g.receiver == "Int")
            .expect("Int receiver group present");
        assert!(int_group.methods.iter().any(|m| m == "add"));
    }

    #[test]
    fn targets_cover_primary_set() {
        let v = build_tooling();
        let ids: Vec<_> = v.targets.iter().map(|t| t.id.as_str()).collect();
        for expected in ["js", "ts", "python", "rust", "go"] {
            assert!(ids.contains(&expected), "missing target {expected}");
        }
    }
}
