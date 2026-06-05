//! Rust code generator — rule-based (Tier 2) transpilation from AIR to Rust.
//!
//! The most direct mapping of any target — Bock's ownership model was designed
//! to map cleanly to Rust:
//!
//! - Owned values → owned values (direct)
//! - Immutable borrow → `&T`
//! - Mutable borrow → `&mut T`
//! - Move → move semantics (direct)
//! - `@managed` → `Rc<T>` (single-threaded) / `Arc<T>` (concurrent)
//! - Records → structs
//! - Enums → enums (with variants)
//! - Traits → traits, Impls → impl blocks (nearly 1:1)
//! - Effects → `&dyn EffectTrait` parameters
//! - Pattern matching → native `match`
//! - Generics → monomorphized (preserved)
//! - String interpolation → `format!()` macro

use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;

use bock_air::{AIRNode, AirInterpolationPart, EnumVariantPayload, NodeKind, ResultVariant};
use bock_ast::{AssignOp, BinOp, Literal, TypeExpr, UnaryOp, Visibility};
use bock_types::AIRModule;

use crate::error::CodegenError;
use crate::generator::{CodeGenerator, GeneratedCode, OutputFile, SourceMap};
use crate::profile::TargetProfile;

/// Prelude container value/type names the Rust backend lowers to **native**
/// Rust (`Optional`/`Result` → `Option`/`Result`; `Some`/`None`/`Ok`/`Err` are
/// native constructors) rather than to a cross-module import. The per-module
/// `use`-emission pass skips these: they are not real exports of the declaring
/// stdlib module, so a `use crate::core::option::Some;` would not resolve. The
/// comparison `Ordering` enum is deliberately **absent** — `core.compare`
/// genuinely declares (`public enum Ordering`) and exports it, so a cross-module
/// use of it resolves through a real `use crate::core::compare::Ordering;`.
const RS_NATIVE_PRELUDE_NAMES: &[&str] = &["Optional", "Result", "Some", "None", "Ok", "Err"];

/// Conservative module scan for `Channel` / `spawn` references.
fn rs_module_uses_concurrency(items: &[AIRNode]) -> bool {
    items.iter().any(|n| {
        let s = format!("{n:?}");
        s.contains("\"Channel\"") || s.contains("\"spawn\"")
    })
}

/// Runtime helpers for Bock concurrency in Rust. Backed by
/// `tokio::sync::mpsc::unbounded_channel`.
const CONCURRENCY_RUNTIME_RS: &str = "\
// ── Bock concurrency runtime ──
use std::sync::Arc;
pub struct __BockChannel<T> {
    tx: tokio::sync::mpsc::UnboundedSender<T>,
    rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<T>>,
}
pub fn __bock_channel_new<T: Send + 'static>() -> (Arc<__BockChannel<T>>, Arc<__BockChannel<T>>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let ch = Arc::new(__BockChannel { tx, rx: tokio::sync::Mutex::new(rx) });
    (ch.clone(), ch)
}
impl<T> __BockChannel<T> {
    pub fn send(&self, v: T) { let _ = self.tx.send(v); }
    pub async fn recv(&self) -> T {
        let mut guard = self.rx.lock().await;
        guard.recv().await.expect(\"channel closed\")
    }
    pub fn close(&self) {}
}
pub fn __bock_spawn<T: Send + 'static>(f: impl std::future::Future<Output = T> + Send + 'static) -> tokio::task::JoinHandle<T> {
    tokio::spawn(f)
}
";

/// Rust code generator implementing the `CodeGenerator` trait.
#[derive(Debug)]
pub struct RsGenerator {
    profile: TargetProfile,
}

impl RsGenerator {
    /// Creates a new Rust code generator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: TargetProfile::rust(),
        }
    }
}

impl Default for RsGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl CodeGenerator for RsGenerator {
    fn target(&self) -> &TargetProfile {
        &self.profile
    }

    fn generate_module(&self, module: &AIRModule) -> Result<GeneratedCode, CodegenError> {
        // Shared pre-pass: hoist value-position diverging control flow (see
        // `hoist_value_cf`) into declare-then-assign temp blocks.
        let module = &crate::generator::hoist_value_cf(module.clone());
        let mut ctx = RsEmitCtx::new();
        ctx.enum_variants =
            crate::generator::collect_enum_variants(&[(module, std::path::Path::new(""))]);
        ctx.generic_decls =
            crate::generator::collect_generic_decls(&[(module, std::path::Path::new(""))]);
        ctx.collect_clone_targets(module);
        ctx.collect_fn_returning_fns(module);
        let trait_decls =
            crate::generator::collect_trait_decls(&[(module, std::path::Path::new(""))]);
        ctx.collect_self_operand_methods(&trait_decls);
        ctx.trait_decls = trait_decls;
        ctx.emit_node(module)?;
        let content = ctx.finish();
        let source_map = SourceMap {
            generated_file: String::new(),
            ..Default::default()
        };
        Ok(GeneratedCode {
            files: vec![OutputFile {
                path: PathBuf::new(),
                content,
                source_map: Some(source_map),
            }],
        })
    }

    /// Emit a per-module **native Rust module tree** (spec §20.6.1; DQ19
    /// resolved): each module the entry program reaches through a real `use` is
    /// emitted to its **own** `.rs` file under `src/`, wired with Rust's native
    /// module system (`mod <m>;` declarations + `use crate::<m>::<x>;` for
    /// cross-module references). This is the sole `bock build` output path.
    ///
    /// ## Layout (cargo-idiomatic `src/`-rooted crate)
    ///
    /// Codegen emits the `src/`-rooted source tree (in all modes); in project
    /// mode the scaffolder adds the `Cargo.toml` run affordance (S6a / DV18), so
    /// `build/rust/` becomes a runnable Cargo crate:
    /// - `Cargo.toml` — minimal manifest (`[package]` + a `[[bin]]` at
    ///   `src/main.rs`), emitted by the **scaffolder** in project mode only —
    ///   just enough to `cargo run`.
    /// - `src/main.rs` — the entry module's body, preceded by `mod <seg>;`
    ///   declarations for every top-level namespace the tree contains.
    /// - `src/<path>.rs` — one file per reached non-entry module, mirrored from
    ///   its **declared** module-path (`module core.option` ⇒ `src/core/option.rs`).
    /// - `src/<namespace>.rs` — a wiring file per intermediate namespace
    ///   (`src/core.rs` declaring `pub mod option; pub mod iter; …`), since Rust
    ///   requires every file be reached through a `mod` declaration.
    ///
    /// §20.6.1 allows "the target ecosystem's conventions," so the `src/`-rooted
    /// mirror is the correct idiomatic layout. The crate is run via `cargo run`
    /// from `build/rust/` (debug build — see the rust run plan in
    /// `bock-build`'s `toolchain.rs`).
    ///
    /// ## Cross-module references
    ///
    /// Each emitted file lists its cross-module dependencies as
    /// `use crate::<declared::path>::<symbol>;` at the top — both the explicit
    /// `use`d symbols and the implicit §18.2-prelude names a module references
    /// but does not `use` (e.g. a base trait in an `impl`). The symbols are then
    /// referenced unqualified in the body exactly as the bundling path emitted
    /// them, so the per-item lowering is unchanged.
    ///
    /// The concurrency runtime (used by `Channel`/`spawn` programs) is emitted
    /// **once** into a shared `src/bock_runtime.rs`; modules referencing it
    /// `use crate::bock_runtime::*;`. Rust uses a native `fn main`, so no entry
    /// invocation is appended.
    fn generate_project(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
    ) -> Result<GeneratedCode, CodegenError> {
        // Shared pre-pass: hoist value-position diverging control flow on every
        // module before registry collection or emission (see `hoist_value_cf`).
        let hoisted: Vec<(AIRModule, &std::path::Path)> = modules
            .iter()
            .map(|(m, p)| (crate::generator::hoist_value_cf((*m).clone()), *p))
            .collect();
        let modules: Vec<(&AIRModule, &std::path::Path)> =
            hoisted.iter().map(|(m, p)| (m, *p)).collect();
        let modules = modules.as_slice();
        // Emit only modules the entry program actually `use`s (plus the entry
        // itself), dependency-ordered — never the prelude-only stdlib.
        let reachable = crate::generator::reachable_modules(modules);
        let modules = reachable.as_slice();
        if modules.is_empty() {
            return Ok(GeneratedCode { files: vec![] });
        }

        let entry_idx = modules
            .iter()
            .position(|(m, _)| crate::generator::module_declares_main_fn(m))
            .unwrap_or(modules.len() - 1);

        // Registries collected across the whole reachable set so a reference in
        // one file to a type/variant/trait declared in another lowers
        // identically to the bundling path.
        let enum_variants = crate::generator::collect_enum_variants(modules);
        let generic_decls = crate::generator::collect_generic_decls(modules);
        let trait_decls = crate::generator::collect_trait_decls(modules);
        let public_symbols = crate::generator::collect_public_symbol_modules(modules);

        // Map each trait's method names → (declaring module-path, trait name).
        // A cross-module call of a trait method (`x.message()` where `Error` is
        // declared in `core.error`) requires the *trait* to be in scope in Rust
        // (`use crate::core::error::Error;`) — the bundling path had it for free
        // (one crate root). The implicit-import scan only sees the *method* name
        // (`message`) referenced, not the trait, so this lets the per-module
        // emitter import the trait when its method is used. Built from the trait
        // registry + the public-symbol map (which carries the declaring module).
        let mut trait_method_owner: HashMap<String, (String, String)> = HashMap::new();
        for (trait_name, info) in &trait_decls {
            let Some(module_path) = public_symbols.get(trait_name) else {
                continue; // a non-public / local-only trait needs no cross-module import
            };
            for m in &info.methods {
                if let NodeKind::FnDecl { name, .. } = &m.kind {
                    trait_method_owner
                        .insert(name.name.clone(), (module_path.clone(), trait_name.clone()));
                }
            }
        }

        // Map each enum *variant* name → (declaring module-path, enum name). The
        // Rust backend qualifies a variant as `Enum::Variant`, so a module that
        // *constructs or matches* a cross-module enum's variant needs the **enum
        // type** in scope (`use crate::core::compare::Ordering;`) — but the AIR
        // it references names the *variant* (`Greater`), not the enum, so the
        // plain implicit-import scan (which keys on the public *enum* name)
        // misses it. This drives that import from a referenced variant. Built
        // from the cross-module variant registry + the public-symbol map (which
        // carries the enum's declaring module). Built-in Optional/Result/Ordering
        // pre-seeds in the registry whose enum is not a real public symbol are
        // skipped (they lower natively, not through a `use`).
        let mut variant_enum_owner: HashMap<String, (String, String)> = HashMap::new();
        for (variant, info) in &enum_variants {
            if let Some(module_path) = public_symbols.get(&info.enum_name) {
                variant_enum_owner.insert(
                    variant.clone(),
                    (module_path.clone(), info.enum_name.clone()),
                );
            }
        }

        // Self-operand + clone-target sets are global to the program (a generic
        // helper in one module may take a clone-bound record from another), so
        // collect them once into a template ctx and clone into each per-module
        // ctx below.
        let mut template = RsEmitCtx::new();
        template.enum_variants = enum_variants;
        template.generic_decls = generic_decls;
        template.collect_self_operand_methods(&trait_decls);
        template.trait_decls = trait_decls;
        for (module, _) in modules {
            template.collect_clone_targets(module);
            template.collect_fn_returning_fns(module);
        }
        // Effect-op resolution needs the whole reachable set: a bare op in one
        // module may belong to an effect declared in another (cross-module
        // effects, §10 + DV13).
        template.seed_effect_registries(modules);

        // The non-entry reached module-paths, for the `mod`-tree wiring.
        let mut tree_paths: Vec<String> = Vec::new();
        let mut needs_runtime = false;

        let mut files: Vec<OutputFile> = Vec::with_capacity(modules.len() + 3);
        for (i, (module, source_path)) in modules.iter().enumerate() {
            let own_path = crate::generator::module_path_string(module).unwrap_or_default();
            let mut ctx = template.fork();
            ctx.per_module = true;
            let mut imports =
                crate::generator::implicit_imports_for(module, &public_symbols, &own_path);
            // Also import a cross-module trait whose *method* this module calls
            // (`x.message()` ⇒ `use crate::core::error::Error;`), so the trait is
            // in scope for method resolution. Conservative: a structural scan for
            // the method name as a quoted identifier; a dead `use` is harmless
            // (`#![allow(unused_imports)]`).
            let rendered = format!("{module:?}");
            for (method, (trait_module, trait_name)) in &trait_method_owner {
                if trait_module == &own_path {
                    continue; // trait declared locally — already in scope
                }
                if rendered.contains(&format!("\"{method}\"")) {
                    imports.push((trait_module.clone(), trait_name.clone()));
                }
            }
            // And import the *enum type* whose cross-module *variant* this module
            // constructs/matches (`Ordering::Greater` ⇒ `use
            // crate::core::compare::Ordering;`) — the AIR names the variant, but
            // Rust qualifies it through the enum, which must be in scope.
            for (variant, (enum_module, enum_name)) in &variant_enum_owner {
                if enum_module == &own_path {
                    continue; // enum declared locally — already in scope
                }
                if rendered.contains(&format!("\"{variant}\"")) {
                    imports.push((enum_module.clone(), enum_name.clone()));
                }
            }
            ctx.implicit_imports = imports;
            ctx.emit_node(module)?;
            needs_runtime |= ctx.concurrency_runtime_emitted;
            let body = ctx.finish_per_module();

            // The entry module's body is `src/main.rs` (preceded by the
            // `mod`-tree declarations, prepended below); every other module is
            // placed under `src/` at its declared-path mirror.
            let rel = if i == entry_idx {
                PathBuf::from("main.rs")
            } else {
                tree_paths.push(own_path.clone());
                crate::generator::module_tree_relpath(module, source_path, self.target())
            };
            let out_path = PathBuf::from("src").join(&rel);
            let generated_file = out_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            files.push(OutputFile {
                path: out_path,
                content: body,
                source_map: Some(SourceMap {
                    generated_file,
                    ..Default::default()
                }),
            });
        }

        // Build the `mod`-tree: `main.rs` gets `mod <top>;` for each top-level
        // namespace; each intermediate namespace gets a `src/<ns>.rs` wiring
        // file declaring `pub mod <child>;`. Add the shared runtime module too.
        let mut tree = ModTree::default();
        for p in &tree_paths {
            tree.insert(p);
        }
        if needs_runtime {
            tree.insert("bock_runtime");
        }
        let root_mods = tree.root_decls();
        for (wiring_rel, decls) in tree.wiring_files() {
            files.push(OutputFile {
                path: PathBuf::from("src").join(&wiring_rel),
                content: decls,
                source_map: None,
            });
        }

        // Insert the root `mod` declarations into `src/main.rs` *after* the
        // leading `#![allow(...)]` inner attribute (an inner attribute must
        // precede every item, so the `mod`s cannot go before it). They are
        // placed at the top of the item region, ahead of the cross-module `use`s
        // and the body.
        if !root_mods.is_empty() {
            if let Some(main_file) = files
                .iter_mut()
                .find(|f| f.path == PathBuf::from("src").join("main.rs"))
            {
                let block = format!("{root_mods}\n");
                // The inner-attribute prefix ends at the first blank line after
                // the `#![allow(...)]` line; insert the `mod`s right there. If
                // (defensively) no inner attribute is present, prepend.
                match main_file.content.find("]\n\n") {
                    Some(idx) => {
                        let at = idx + "]\n\n".len();
                        main_file.content.insert_str(at, &block);
                    }
                    None => main_file.content.insert_str(0, &block),
                }
            }
        }

        // Shared concurrency runtime module (tokio-backed), emitted once.
        if needs_runtime {
            files.push(OutputFile {
                path: PathBuf::from("src").join("bock_runtime.rs"),
                content: format!("#![allow(unused_imports, dead_code)]\n{CONCURRENCY_RUNTIME_RS}"),
                source_map: None,
            });
        }

        // Manifest emission moved to the project-mode scaffolder (S6a / DV18):
        // codegen emits only the per-module *source* tree in all modes; the
        // `Cargo.toml` run affordance is emitted by `RustScaffolder` in project
        // mode only (never under `--source-only`). See `scaffold.rs`.

        Ok(GeneratedCode { files })
    }

    /// Transpile `@test` functions into an inline `#[cfg(test)] mod` (S7).
    ///
    /// `cargo test` runs the bin crate's inline test module. Each Bock `@test`
    /// becomes a `#[test] fn`, with `expect(actual).<assertion>(expected)` chains
    /// lowered to `assert!` / `assert_eq!`. The module is emitted to
    /// `src/bock_tests.rs` and wired into `src/main.rs` via the returned
    /// `entry_append` (`#[cfg(test)] mod bock_tests;`). `framework` is ignored:
    /// `cargo test` is the universal Rust framework (§20.6.2).
    fn generate_tests(
        &self,
        modules: &[(&AIRModule, &std::path::Path)],
        _framework: &str,
    ) -> Result<crate::generator::TestArtifacts, CodegenError> {
        let reachable = crate::generator::reachable_modules(modules);
        let modules = reachable.as_slice();
        let tests = crate::generator::collect_test_fns(modules);
        if tests.is_empty() {
            return Ok(crate::generator::TestArtifacts::default());
        }

        // Build the same cross-module registries `generate_project` uses so the
        // test bodies lower references (enum variants, generics, trait methods)
        // identically to the runtime tree.
        let enum_variants = crate::generator::collect_enum_variants(modules);
        let generic_decls = crate::generator::collect_generic_decls(modules);
        let trait_decls = crate::generator::collect_trait_decls(modules);
        let mut template = RsEmitCtx::new();
        template.enum_variants = enum_variants;
        template.generic_decls = generic_decls;
        template.collect_self_operand_methods(&trait_decls);
        template.trait_decls = trait_decls;
        for (module, _) in modules {
            template.collect_clone_targets(module);
            template.collect_fn_returning_fns(module);
        }
        template.seed_effect_registries(modules);

        // The test module lives at the crate root; `use super::*` brings in
        // everything the bin's `main.rs` (the entry module) declares. Each
        // *non-entry* reachable module is a real `crate::<ns>` submodule, so
        // bring each top-level namespace in too, letting a test call functions
        // declared in a `use`d module. (The entry module is the crate root, not
        // a `crate::<entry>` submodule, so it must NOT be added here.)
        let entry_idx = modules
            .iter()
            .position(|(m, _)| crate::generator::module_declares_main_fn(m))
            .unwrap_or(modules.len() - 1);
        let mut ctx = template.fork();
        ctx.per_module = true;
        ctx.indent = 0;
        ctx.writeln("use super::*;");
        let mut namespaces: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (i, (module, _)) in modules.iter().enumerate() {
            if i == entry_idx {
                continue;
            }
            if let Some(p) = crate::generator::module_path_string(module) {
                if let Some(top) = p.split('.').next() {
                    if !top.is_empty() {
                        namespaces.insert(top.to_string());
                    }
                }
            }
        }
        for ns in &namespaces {
            ctx.writeln(&format!("use crate::{ns}::*;"));
        }

        for (test_fn, _module_path) in &tests {
            let NodeKind::FnDecl { name, body, .. } = &test_fn.kind else {
                continue;
            };
            ctx.buf.push('\n');
            ctx.writeln("#[test]");
            ctx.writeln(&format!("fn {}() {{", to_snake_case(&name.name)));
            ctx.indent += 1;
            ctx.emit_test_body(body)?;
            ctx.indent -= 1;
            ctx.writeln("}");
        }

        // `src/bock_tests.rs` IS the `bock_tests` module body (it is reached via
        // `#[cfg(test)] mod bock_tests;` in `main.rs`), so the `use`/`#[test] fn`
        // items go at file top with NO extra `mod` wrapper — otherwise `super`
        // would resolve to a spurious inner module instead of the crate root and
        // `use super::*` would not bring in the bin's items. The whole file is
        // already `#[cfg(test)]`-gated by the `mod bock_tests;` declaration.
        let body = ctx.buf;
        let content = format!("#![allow(unused_imports, unused_parens, dead_code)]\n\n{body}");

        Ok(crate::generator::TestArtifacts {
            files: vec![OutputFile {
                path: PathBuf::from("src").join("bock_tests.rs"),
                content,
                source_map: None,
            }],
            entry_append: Some("\n#[cfg(test)]\nmod bock_tests;\n".to_string()),
        })
    }
}

/// Builder for the Rust `mod` declaration tree of a per-module crate.
///
/// Rust requires every source file be reached through a `mod`/`pub mod`
/// declaration from the crate root. Given the set of reached non-entry
/// module-paths (`core.option`, `helper`, `bock_runtime`, …), this produces:
/// - the root declarations for `src/main.rs` (`mod core;`, `mod helper;`, … —
///   one per distinct top-level namespace), and
/// - one wiring file per intermediate namespace (`src/core.rs` declaring
///   `pub mod option;`, `pub mod iter;`, …).
///
/// All v1 modules are leaves under a namespace (`core.X`) or bare roots
/// (`helper`), but the builder handles an arbitrarily deep tree.
#[derive(Default)]
struct ModTree {
    /// Child namespaces keyed by dotted prefix. The empty string is the crate
    /// root; `core` maps to the children declared in `src/core.rs`. Values are
    /// the immediate child segment names (deduped, sorted on render).
    children: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
}

impl ModTree {
    /// Register a reached module-path, recording every parent→child edge so the
    /// crate root and each intermediate namespace declare the right submodules.
    fn insert(&mut self, dotted: &str) {
        let segs: Vec<&str> = dotted.split('.').filter(|s| !s.is_empty()).collect();
        let mut prefix = String::new();
        for seg in &segs {
            self.children
                .entry(prefix.clone())
                .or_default()
                .insert((*seg).to_string());
            if prefix.is_empty() {
                prefix = (*seg).to_string();
            } else {
                prefix.push('.');
                prefix.push_str(seg);
            }
        }
    }

    /// The crate-root `mod <seg>;` declarations (for `src/main.rs`), one per
    /// distinct top-level namespace, newline-terminated. Empty when the program
    /// has no cross-module dependencies.
    fn root_decls(&self) -> String {
        let mut out = String::new();
        if let Some(roots) = self.children.get("") {
            for seg in roots {
                out.push_str(&format!("mod {seg};\n"));
            }
        }
        out
    }

    /// One wiring file per intermediate namespace: `(relative path, contents)`
    /// where the path is `<namespace>.rs` (e.g. `core.rs`) and the contents are
    /// its `pub mod <child>;` declarations. Excludes the crate root (whose decls
    /// go in `main.rs` via [`Self::root_decls`]).
    fn wiring_files(&self) -> Vec<(PathBuf, String)> {
        let mut files = Vec::new();
        for (prefix, kids) in &self.children {
            if prefix.is_empty() {
                continue;
            }
            let mut content = String::new();
            for kid in kids {
                content.push_str(&format!("pub mod {kid};\n"));
            }
            let rel: PathBuf = prefix.split('.').collect::<PathBuf>().with_extension("rs");
            files.push((rel, content));
        }
        files
    }
}

// ─── Emission context ────────────────────────────────────────────────────────

/// Internal state for Rust emission.
struct RsEmitCtx {
    buf: String,
    indent: usize,
    /// Track whether we need `use std::rc::Rc;` at the top.
    needs_rc_import: bool,
    /// Track whether we need `use std::sync::Arc;` at the top.
    needs_arc_import: bool,
    /// Names bound in the current block whose Call value is wrapped in
    /// `tokio::spawn(...)` because the binding is later `await`ed within the
    /// same block. Rust futures are lazy, so without this, sequential
    /// `.await` on each binding would serialise the work. See
    /// [`Self::collect_task_bindings`].
    task_bound_names: std::collections::HashSet<String>,
    /// Maps effect operation name → effect type name (e.g., "log" → "Logger").
    effect_ops: HashMap<String, String>,
    /// Maps effect type name → current handler variable name in scope.
    current_handler_vars: HashMap<String, String>,
    /// Effect type names whose in-scope handler variable is *already a reference*
    /// (`&impl Effect`) — i.e. an effectful function's own `&impl Effect`
    /// parameter forwarded to a nested effectful call. Forwarding such a handler
    /// must pass it *as-is* (`handler`), not re-borrowed (`&handler`), which would
    /// be `&&impl Effect` and fail the `Effect` trait bound (`E0277`). A handler
    /// that is a concrete owned value instead (module-level `handle` const, a
    /// `handling`-block local) is NOT in this set and is forwarded as `&handler`.
    /// Saved/restored alongside [`Self::current_handler_vars`] at every scope that
    /// rebinds handlers.
    borrowed_handler_effects: std::collections::HashSet<String>,
    /// Maps function name → effect type names from its `with` clause.
    fn_effects: HashMap<String, Vec<String>>,
    /// Maps composite effect name → component effect names.
    composite_effects: HashMap<String, Vec<String>>,
    /// Set once the concurrency runtime prelude has been emitted in the
    /// single-module self-contained path ([`RustGenerator::generate_module`]), so
    /// a module referencing it more than once still inlines it at most once (a
    /// duplicate `struct __BockChannel` is a Rust redefinition error). The
    /// per-module project path emits the runtime once into a shared module.
    concurrency_runtime_emitted: bool,
    /// User-enum-variant registry (DV14). Maps a variant name to its enum so a
    /// construction (`Circle { .. }`, `Rect(..)`, `Empty`) and a match pattern
    /// can be qualified `Enum::Variant`, which Rust requires (an unqualified
    /// variant does not resolve at the crate root). Pre-scanned across the
    /// reached modules; consulted *after* the bespoke Optional/Result paths so
    /// those are never regressed.
    enum_variants: crate::generator::EnumVariantRegistry,
    /// Generic-type declaration registry: a record/enum/class name → its
    /// declared generic params. An `impl Box { ... }` block carries no params of
    /// its own (the `T` is declared on `record Box[T]`); Rust requires the impl
    /// to introduce and apply them (`impl<T> Box<T> { ... }`). This recovers them
    /// at the impl site. Pre-scanned across the reached modules (mirrors
    /// [`Self::enum_variants`]).
    generic_decls: crate::generator::GenericDeclRegistry,
    /// Records whose `impl` returns a `self` field by value and so need
    /// `#[derive(Clone)]` plus a `T: Clone` bound on the generic impl (a `&self`
    /// method cannot move a non-`Copy` field out, so the field read is lowered
    /// to `self.field.clone()`). Populated by [`Self::collect_clone_targets`]
    /// before emission so the `RecordDecl` can decide whether to derive `Clone`.
    clone_target_records: std::collections::HashSet<String>,
    /// Names of *generic* records whose inherent or trait `impl` will carry a
    /// `T: Clone` bound — either because they return a `self` field by value
    /// ([`Self::clone_target_records`]) or because a method clones a generic
    /// collection element ([`Self::body_clones_collection_element`], e.g.
    /// `ListIterator.next` doing `self.xs.get(self.cursor)`). A free generic
    /// function that takes such a record by value and calls a method on it
    /// (`count[T](it: ListIterator[T])` driving `it.next()`) must propagate the
    /// bound, or method resolution fails (`E0599`: trait bounds not satisfied).
    /// Populated by [`Self::collect_clone_targets`].
    clone_bound_records: std::collections::HashSet<String>,
    /// True while emitting a method body whose impl target is generic and clones
    /// `self` fields. Gates the `self.field` → `self.field.clone()` rewrite so it
    /// applies only inside such methods (never to general field reads, which
    /// would be noisy and could over-require `Clone`).
    in_clone_self_method: bool,
    /// True while emitting the **target** (LHS) of an assignment
    /// (`self.cursor = …`). Suppresses the [`Self::in_clone_self_method`]
    /// `self.field` → `self.field.clone()` rewrite there: an assignment target is
    /// a place expression, and `self.cursor.clone() = …` is not valid Rust. Set
    /// and cleared around the target emit in the `Assign` arm.
    in_assign_target: bool,
    /// Names of trait methods whose non-receiver operand is `Self`-typed
    /// (`compare`/`eq`/`beats`/…). Such an operand is emitted and *called* by
    /// shared reference in Rust: the trait/impl signature is `other: &Self` /
    /// `other: &Target`, and a desugared call borrows the argument
    /// (`a.compare(&b)`). Bock's value semantics permit reusing the argument
    /// after the call (e.g. stdlib `max` does `match a.compare(b) { _ => b }`),
    /// which by-value would move a non-`Copy` value out (Rust E0382). Derived
    /// from the trait registry; keyed by the bare method name (globally unique
    /// within a v1 program).
    self_operand_methods: std::collections::HashSet<String>,
    /// Names of match-pattern bindings in the current arm that are *used more
    /// than once* in the arm body. Such a binding (`Some(x) => ... pred(x) ...
    /// [x] ...`) is moved by its first by-value consumer (the Rust pattern
    /// binds by value), so each later by-value use must clone to keep the value
    /// live (`E0382`: use of moved value). When a bare-identifier call argument
    /// names a binding in this set, codegen emits `x.clone()` rather than `x`.
    /// The clone is always valid: a generic such binding is element-typed and
    /// its fn already carries the matching `T: Clone` bound (e.g.
    /// `filter[T](.., pred: Fn(T) -> Bool)`), and concrete v1 element types are
    /// `Clone`. Saved/restored around each arm so it never leaks across arms.
    reused_match_bindings: std::collections::HashSet<String>,
    /// Snake-cased names of `let`-bound variables in the current block that are
    /// read by-value more than once (a non-`Copy` value passed by value to a
    /// free function is *moved* by the first consumer, so a later by-value pass
    /// is `E0382`). A bare-identifier free-function argument naming such a
    /// binding is emitted as `x.clone()`. Mirrors [`Self::reused_match_bindings`]
    /// for `let` bindings rather than match-arm bindings: the same move-reuse
    /// hazard arises whenever a query helper (`size(s)`, `contains(s, x)`,
    /// `to_list(s)`) takes a record by value and the binding is queried again.
    /// The clone is sound: a concrete v1 record/collection derives `Clone`, and a
    /// generic such binding lives in a fn already carrying the matching `T:
    /// Clone` bound. Seeded per-block (saved/restored) so it never leaks.
    reused_let_bindings: std::collections::HashSet<String>,
    /// The reached modules' user-declared traits (keyed by name). Used to
    /// distinguish a
    /// `T: Equatable` bound that is a real user trait (it has an `impl`, so the
    /// bound and the `.eq` call dispatch normally) from the compiler-provided
    /// sealed-core conformance, which must be lowered to the Rust std trait /
    /// native operator (GAP-C). See [`crate::generator::is_unimplemented_sealed_core_trait`].
    trait_decls: crate::generator::TraitDeclRegistry,
    /// True in the **per-module native-module** emission path
    /// ([`RustGenerator::generate_project`], the sole real-build path). When set,
    /// the `Module` arm emits real `use crate::<m>::<x>;` for cross-module
    /// references (explicit `use`s and the implicit prelude imports) at the top
    /// of the file instead of dropping the `ImportDecl`s, and the concurrency
    /// runtime is imported from the shared `bock_runtime` module rather than
    /// inlined. When clear, the module is emitted as a single self-contained file
    /// with its runtime preludes inlined — the [`RustGenerator::generate_module`]
    /// path used by unit tests.
    per_module: bool,
    /// Implicit cross-module imports for the per-module path, as
    /// `(module_path, symbol_name)` pairs — public names this module references
    /// but neither declares locally nor imports via an explicit `use` (e.g. a
    /// §18.2-prelude trait used as an `impl` base). The `Module` arm emits a
    /// `use crate::<module_path>::<symbol_name>;` for each. Computed in
    /// `generate_project`.
    implicit_imports: Vec<(String, String)>,
    /// Armed while emitting the body of a function whose declared return type is
    /// a `Fn(..) -> ..` (lowered to `impl Fn`). Promoted to
    /// [`Self::returning_fn_closure`] only for the body's *tail* expression (the
    /// returned value) by `emit_block_body`, so an intermediate `.map`/`.filter`
    /// closure earlier in the body is unaffected.
    return_closure_tail: bool,
    /// Set only while emitting the *tail* expression of a closure-returning
    /// function (see [`Self::return_closure_tail`]). A closure
    /// (`Lambda`/`Compose`) produced here must `move`-capture its environment:
    /// the returned `impl Fn` outlives the function frame, so borrowing a
    /// captured param/local would be a dangling reference (E0373/E0507). The
    /// function's `impl Fn` params additionally gain a `+ 'static` bound so the
    /// moved captures satisfy the `'static` default of the returned `impl Fn`
    /// (E0310).
    returning_fn_closure: bool,
    /// Names (original, not snake-cased) of top-level functions whose declared
    /// return type is a `Fn(..) -> ..` (lowered to `impl Fn`). A `let` binding
    /// whose RHS calls such a function holds a closure value, so it must not be
    /// `.clone()`d on reuse (an `impl Fn` opaque type is not `Clone` — E0599);
    /// it is borrowed instead. Populated once per program by
    /// [`Self::collect_fn_returning_fns`].
    fn_returning_fns: std::collections::HashSet<String>,
    /// Snake-cased names of in-scope `let` bindings whose value is a function /
    /// closure (`impl Fn`) — a `Lambda`/`Compose` RHS, an explicit `Fn(..)`
    /// annotation, or a call to a [`Self::fn_returning_fns`] helper. A move-reuse
    /// pass of such a binding is **borrowed** (`&f`) rather than cloned: `impl
    /// Fn` is not `Clone` (E0599), but `&F: Fn` when `F: Fn`, so a borrow
    /// satisfies an `impl Fn` parameter and leaves the binding live for the next
    /// pass. Seeded per-block (saved/restored) so it never leaks.
    fn_typed_bindings: std::collections::HashSet<String>,
    /// Snake-cased names of in-scope `let` bindings whose value is a Rust
    /// collection (`Vec`/`HashMap`/`HashSet`) — recognised from the binding's
    /// RHS (`map.keys()`, a list literal, …) or a `List`/`Map`/`Set` type
    /// annotation. A `Vec`/`HashMap`/`HashSet` does not implement
    /// `std::fmt::Display`, so an interpolation of such a binding (`"keys=${keys}"`)
    /// must use the `Debug` formatter (`{:?}`) instead of `{}` (E0277). Seeded
    /// per-block in [`Self::emit_block_body`]; saved/restored so it never leaks.
    collection_bindings: std::collections::HashSet<String>,
}

impl RsEmitCtx {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent: 0,
            needs_rc_import: false,
            needs_arc_import: false,
            task_bound_names: std::collections::HashSet::new(),
            effect_ops: HashMap::new(),
            current_handler_vars: HashMap::new(),
            borrowed_handler_effects: std::collections::HashSet::new(),
            fn_effects: HashMap::new(),
            composite_effects: HashMap::new(),
            concurrency_runtime_emitted: false,
            enum_variants: crate::generator::EnumVariantRegistry::new(),
            generic_decls: crate::generator::GenericDeclRegistry::new(),
            clone_target_records: std::collections::HashSet::new(),
            clone_bound_records: std::collections::HashSet::new(),
            in_clone_self_method: false,
            in_assign_target: false,
            self_operand_methods: std::collections::HashSet::new(),
            reused_match_bindings: std::collections::HashSet::new(),
            reused_let_bindings: std::collections::HashSet::new(),
            trait_decls: crate::generator::TraitDeclRegistry::new(),
            per_module: false,
            implicit_imports: Vec::new(),
            return_closure_tail: false,
            returning_fn_closure: false,
            fn_returning_fns: std::collections::HashSet::new(),
            fn_typed_bindings: std::collections::HashSet::new(),
            collection_bindings: std::collections::HashSet::new(),
        }
    }

    /// Clone the cross-module *analysis* state (registries + the global
    /// clone/self-operand sets) into a fresh emission context with an empty
    /// buffer. Used by the per-module path to emit each module file from the
    /// same pre-scanned program-wide context the bundling path built once, so a
    /// reference in one file to a type/trait declared in another lowers
    /// identically. The per-file state (`implicit_imports`, the runtime flag,
    /// the buffer) starts fresh.
    fn fork(&self) -> Self {
        Self {
            buf: String::with_capacity(4096),
            indent: 0,
            needs_rc_import: false,
            needs_arc_import: false,
            task_bound_names: std::collections::HashSet::new(),
            effect_ops: self.effect_ops.clone(),
            current_handler_vars: HashMap::new(),
            borrowed_handler_effects: std::collections::HashSet::new(),
            fn_effects: self.fn_effects.clone(),
            composite_effects: self.composite_effects.clone(),
            concurrency_runtime_emitted: false,
            enum_variants: self.enum_variants.clone(),
            generic_decls: self.generic_decls.clone(),
            clone_target_records: self.clone_target_records.clone(),
            clone_bound_records: self.clone_bound_records.clone(),
            in_clone_self_method: false,
            in_assign_target: false,
            self_operand_methods: self.self_operand_methods.clone(),
            reused_match_bindings: std::collections::HashSet::new(),
            reused_let_bindings: std::collections::HashSet::new(),
            trait_decls: self.trait_decls.clone(),
            per_module: false,
            implicit_imports: Vec::new(),
            return_closure_tail: false,
            returning_fn_closure: false,
            fn_returning_fns: self.fn_returning_fns.clone(),
            fn_typed_bindings: std::collections::HashSet::new(),
            collection_bindings: std::collections::HashSet::new(),
        }
    }

    /// Populate [`Self::self_operand_methods`] from a trait registry: every
    /// method (in any trait) whose own non-receiver params include a
    /// `Self`-typed operand. These methods take that operand by shared
    /// reference in Rust (see the field doc).
    fn collect_self_operand_methods(&mut self, registry: &crate::generator::TraitDeclRegistry) {
        for info in registry.values() {
            for m in &info.methods {
                let NodeKind::FnDecl { params, name, .. } = &m.kind else {
                    continue;
                };
                let has_self_operand = params.iter().skip(1).any(|p| {
                    matches!(
                        &p.kind,
                        NodeKind::Param { ty: Some(t), .. } if matches!(t.kind, NodeKind::TypeSelf)
                    )
                });
                if has_self_operand {
                    self.self_operand_methods.insert(name.name.clone());
                }
            }
        }
    }

    /// Pre-scan a module's `impl` blocks and mark each *generic* record whose
    /// impl returns a `self` field by value — those need `#[derive(Clone)]` and
    /// a `T: Clone` impl bound because a `&self` method cannot move a non-`Copy`
    /// field out. Returning `self.field` (Bock's by-value receiver consuming a
    /// field) is lowered to `self.field.clone()`. Only generic targets are
    /// considered: a concrete record returning a non-`Copy` field is the
    /// pre-existing, orthogonal `&self` move-out defect, left untouched here.
    fn collect_clone_targets(&mut self, module: &AIRModule) {
        let NodeKind::Module { items, .. } = &module.kind else {
            return;
        };
        for item in items {
            let NodeKind::ImplBlock {
                target, methods, ..
            } = &item.kind
            else {
                continue;
            };
            let target_name = self.type_expr_to_string(target);
            // Only generic targets (the `impl<T> Box<T>` synthesis case).
            let is_generic = self
                .generic_decls
                .get(&target_name)
                .is_some_and(|p| !p.is_empty());
            if !is_generic {
                continue;
            }
            let returns_self_field = methods.iter().any(Self::method_returns_self_field);
            if returns_self_field {
                self.clone_target_records.insert(target_name.clone());
            }
            // Record every generic record whose impl will carry a `T: Clone`
            // bound, so a free generic fn taking it by value and driving its
            // methods can propagate the bound (see `clone_bound_records`). This
            // mirrors the impl-site `add_clone_bound` predicate: a field-return
            // getter, a `self.field` move-out, or a generic-collection-element
            // clone (`ListIterator.next` doing `self.xs.get(...)`).
            let needs_clone_bound = returns_self_field
                || methods.iter().any(|m| {
                    matches!(&m.kind, NodeKind::FnDecl { body, .. }
                        if Self::body_moves_self_field(body)
                            || Self::body_clones_collection_element(body))
                });
            if needs_clone_bound {
                self.clone_bound_records.insert(target_name);
            }
        }
    }

    /// Populate [`Self::fn_returning_fns`] with the names of top-level functions
    /// whose declared return type is a `Fn(..) -> ..` (lowered to a non-`Clone`
    /// `impl Fn`). A `let` binding whose RHS calls such a function (e.g.
    /// `let pipeline = build_report_pipeline()`) then holds a closure value and
    /// must be borrowed, not cloned, on a move-reuse (E0599 — `impl Fn` is not
    /// `Clone`). See [`Self::fn_typed_bindings`].
    fn collect_fn_returning_fns(&mut self, module: &AIRModule) {
        let NodeKind::Module { items, .. } = &module.kind else {
            return;
        };
        for item in items {
            if let NodeKind::FnDecl {
                name, return_type, ..
            } = &item.kind
            {
                if return_type
                    .as_deref()
                    .is_some_and(|t| matches!(&t.kind, NodeKind::TypeFunction { .. }))
                {
                    self.fn_returning_fns.insert(name.name.clone());
                }
            }
        }
    }

    /// True when a `let`-binding RHS produces a function / closure value (`impl
    /// Fn`) — a `Lambda`, a `Compose` (`f >> g`), or a call to a
    /// [`Self::fn_returning_fns`] helper. Such a binding is borrowed (not cloned)
    /// on a move-reuse. A conservative syntactic probe; when unsure it returns
    /// `false` (the binding keeps the default clone-or-move path).
    fn rhs_is_fn_valued(&self, value: &AIRNode) -> bool {
        match &value.kind {
            NodeKind::Lambda { .. } | NodeKind::Compose { .. } => true,
            NodeKind::Call { callee, .. } => {
                matches!(&callee.kind, NodeKind::Identifier { name }
                    if self.fn_returning_fns.contains(&name.name))
            }
            _ => false,
        }
    }

    /// True when a method's body returns a bare `self.field` by value — either an
    /// explicit `return self.field` or a `self.field` block-tail. Such a return
    /// moves the field out of the `&self` receiver and so requires a clone (and a
    /// `Clone` bound) under Rust's borrow rules.
    fn method_returns_self_field(method: &AIRNode) -> bool {
        let NodeKind::FnDecl { body, .. } = &method.kind else {
            return false;
        };
        Self::block_returns_self_field(body)
    }

    /// Does this node, in value/return position, evaluate to a `self.field`?
    fn block_returns_self_field(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::Block { stmts, tail } => {
                if let Some(t) = tail {
                    // The tail may be a bare `self.field` (implicit return) or a
                    // `return self.field;` statement (Bock allows an explicit
                    // `return` in tail position).
                    if Self::is_self_field(t) || Self::stmt_returns_self_field(t) {
                        return true;
                    }
                }
                stmts.iter().any(Self::stmt_returns_self_field)
            }
            _ => Self::is_self_field(node),
        }
    }

    /// A `return self.field;` statement (or a nested block/return that does).
    fn stmt_returns_self_field(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::Return { value: Some(v) } => Self::is_self_field(v),
            NodeKind::Block { .. } => Self::block_returns_self_field(node),
            _ => false,
        }
    }

    /// True when `node` is exactly `self.<field>`.
    fn is_self_field(node: &AIRNode) -> bool {
        matches!(
            &node.kind,
            NodeKind::FieldAccess { object, .. }
                if matches!(&object.kind, NodeKind::Identifier { name } if name.name == "self")
        )
    }

    /// True when this fn/method body, in value/return position, evaluates to an
    /// expression that *contains* a `self.field` read — either a bare
    /// `self.field` or a `self.field` wrapped in a constructor such as
    /// `Some(self.field)` / `Ok(self.field)` / a record or enum-variant build.
    ///
    /// Such a return moves the field out of the `&self` receiver, which Rust's
    /// borrow checker forbids for a non-`Copy` field; the codegen lowers the
    /// `self.field` read to `self.field.clone()` (gated on
    /// [`Self::in_clone_self_method`]) and the impl/fn carries a `T: Clone`
    /// bound. This generalises [`Self::block_returns_self_field`] (a *bare*
    /// `return self.field`) to the wrapped case `return Some(self.v)`, the shape
    /// a generic `fn f(self) -> Optional[T]` produces.
    ///
    /// Crucially it inspects only return/tail *value* positions, never a
    /// statement such as `self.cursor = self.cursor + 1` (whose `self.cursor`
    /// reads must NOT be cloned — the assignment LHS would become an invalid
    /// `self.cursor.clone() = ...`).
    fn body_moves_self_field(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::Block { stmts, tail } => {
                if let Some(t) = tail {
                    if Self::expr_contains_self_field(t) || Self::body_moves_self_field(t) {
                        return true;
                    }
                }
                stmts.iter().any(Self::body_moves_self_field)
            }
            NodeKind::Return { value: Some(v) } => Self::expr_contains_self_field(v),
            // A `let x = … self.field …` RHS moves the field by value out of the
            // `&self` receiver just as a return does (`E0507`) — e.g.
            // `let tag = type_tag(self.msg_type)` in a trait `serialize(self)`.
            // The RHS is a value position (never an assignment LHS), so cloning
            // the `self.field` read there is sound.
            NodeKind::LetBinding { value, .. } => Self::expr_contains_self_field(value),
            // A bare free-function call statement that passes `self.field` by
            // value (`emit(self.payload)`) moves it out too. A `MethodCall` is
            // excluded: its `self.field.method()` receiver *borrows* (methods
            // lower to `&self`), so it is not a move. `Assign` is excluded as
            // well — its target is a place expression whose `self.field` must NOT
            // clone (the `in_assign_target` guard also defends the emit site).
            NodeKind::Call { .. } => Self::expr_contains_self_field(node),
            // Control-flow whose arms carry value/return positions worth
            // descending into (e.g. a `match` whose arms `return Some(self.v)`).
            NodeKind::If {
                then_block,
                else_block,
                ..
            } => {
                Self::body_moves_self_field(then_block)
                    || else_block
                        .as_ref()
                        .is_some_and(|e| Self::body_moves_self_field(e))
            }
            NodeKind::Match { arms, .. } => arms.iter().any(|arm| {
                if let NodeKind::MatchArm { body, .. } = &arm.kind {
                    Self::expr_contains_self_field(body) || Self::body_moves_self_field(body)
                } else {
                    false
                }
            }),
            _ => false,
        }
    }

    /// True when `node` (an expression in value position) reads a `self.field`
    /// directly or via a wrapping constructor call / aggregate. Deliberately
    /// conservative: it descends through `Call` arguments (the `Some(self.v)`
    /// case) and record/aggregate fields, but treats the read as a move only
    /// when it is genuinely a `self.field` access, not e.g. `self.field.method()`
    /// (a method call borrows rather than moves) or a comparison.
    fn expr_contains_self_field(node: &AIRNode) -> bool {
        if Self::is_self_field(node) {
            return true;
        }
        match &node.kind {
            // `Some(self.v)`, `Ok(self.v)`, `Variant(self.v)`, `f(self.v)` — the
            // field flows by value into the constructed/returned value.
            NodeKind::Call { args, .. } => args
                .iter()
                .any(|a| Self::expr_contains_self_field(&a.value)),
            NodeKind::RecordConstruct { fields, .. } => fields.iter().any(|f| {
                f.value
                    .as_deref()
                    .is_some_and(Self::expr_contains_self_field)
            }),
            NodeKind::TupleLiteral { elems } | NodeKind::ListLiteral { elems } => {
                elems.iter().any(Self::expr_contains_self_field)
            }
            _ => false,
        }
    }

    /// True when this fn/method body will emit a `.clone()` / `.cloned()` on a
    /// *generic* element value via a built-in collection method — `List.get` /
    /// `first` / `last` / `concat`, `Map.get` / `keys` / `values`, or a `Set`
    /// algebraic op. Each lowers to a `.cloned()` (or `.clone()` for `concat`)
    /// over the receiver's element type; when that element type is a generic
    /// param the impl/fn needs a `T: Clone` bound (the v1 concrete element types
    /// Int/Float/String/Bool all satisfy it).
    ///
    /// Detection is conservative on the *operation* (does the body call a
    /// clone-inducing built-in at all) rather than precisely typing each
    /// receiver's element — for a generic fn/impl over `List[T]`, the element
    /// flowing through these calls is always the generic param. A clone bound on
    /// a generic param that happens not to need it is harmless (every concrete
    /// instantiation in v1 is `Clone`); the gate is correctness, and the
    /// detection never fires for a body that emits no such call.
    fn body_clones_collection_element(body: &AIRNode) -> bool {
        struct CloneScan {
            found: bool,
        }
        impl bock_air::visitor::Visitor for CloneScan {
            fn visit_node(&mut self, node: &AIRNode) {
                if self.found {
                    return;
                }
                if let NodeKind::Call { callee, args, .. } = &node.kind {
                    if let Some((_, method, _)) =
                        crate::generator::desugared_list_method(node, callee, args)
                    {
                        if matches!(method, "get" | "first" | "last" | "concat") {
                            self.found = true;
                            return;
                        }
                    }
                    // The functional combinators all lower through `.iter()
                    // .cloned()` / `.clone().into_iter()`, so they clone the
                    // element type just like `get`/`concat` above.
                    if crate::generator::desugared_list_functional_method(node, callee, args)
                        .is_some()
                    {
                        self.found = true;
                        return;
                    }
                    if let Some((_, method, _)) =
                        crate::generator::desugared_map_method(node, callee, args)
                    {
                        if matches!(method, "get" | "keys" | "values") {
                            self.found = true;
                            return;
                        }
                    }
                    if let Some((_, method, _)) =
                        crate::generator::desugared_set_method(node, callee, args)
                    {
                        if matches!(method, "union" | "intersection" | "difference" | "to_list") {
                            self.found = true;
                            return;
                        }
                    }
                }
                bock_air::visitor::walk_node(self, node);
            }
        }
        let mut scan = CloneScan { found: false };
        bock_air::visitor::Visitor::visit_node(&mut scan, body);
        scan.found
    }

    /// True when some `match` arm in `body` binds a pattern variable the arm
    /// reads **more than once** — the case the runtime move-reuse analysis
    /// ([`Self::reused_match_bindings`]) lowers by emitting `<x>.clone()` on each
    /// by-value use after the first (the Rust pattern binds by value, so the
    /// first by-value consumer moves it; later uses would be `E0382`). When the
    /// reused binding is a *generic* element (`filter[T](o: Optional[T], pred:
    /// Fn(T) -> Bool)` doing `match o { Some(x) => if pred(x) { Some(x) } … }`),
    /// the emitted `x.clone()` needs `T: Clone` in scope, so the enclosing
    /// generic fn must carry the bound — otherwise `E0599`/`E0277`.
    ///
    /// Conservative: it fires on the *shape* (a match arm with a reused binding)
    /// rather than typing each binding, mirroring
    /// [`Self::body_clones_collection_element`]. A `T: Clone` bound on a generic
    /// param that turns out not to need it is harmless (every concrete v1
    /// element type — Int/Float/String/Bool/nested — is `Clone`), and the scan
    /// never fires for a body whose match arms each use their bindings at most
    /// once (`or_else`/`to_list`/`count`/`get_or` over `Optional` all stay
    /// unconstrained, matching the pre-existing behaviour). The caller gates this
    /// on `!generic_params.is_empty()` so a non-generic fn is never touched.
    fn body_reuses_match_binding(body: &AIRNode) -> bool {
        struct ReuseScan {
            found: bool,
        }
        impl bock_air::visitor::Visitor for ReuseScan {
            fn visit_node(&mut self, node: &AIRNode) {
                if self.found {
                    return;
                }
                if let NodeKind::MatchArm { pattern, body, .. } = &node.kind {
                    let mut bound = Vec::new();
                    RsEmitCtx::collect_pattern_binding_names(pattern, &mut bound);
                    for name in &bound {
                        if RsEmitCtx::count_identifier_uses(body, name) > 1 {
                            self.found = true;
                            return;
                        }
                    }
                }
                bock_air::visitor::walk_node(self, node);
            }
        }
        let mut scan = ReuseScan { found: false };
        bock_air::visitor::Visitor::visit_node(&mut scan, body);
        scan.found
    }

    /// True when a *generic* free function takes a parameter whose base type is
    /// a clone-bound record ([`Self::clone_bound_records`] — a record whose
    /// `impl` carries a `T: Clone` bound, e.g. `ListIterator[T]`) and drives it
    /// with at least one method call. Such a function must propagate the
    /// record's `T: Clone` bound to its own signature, or method resolution
    /// fails (`count[T](it: ListIterator[T])` calling `it.next()` →
    /// `E0599`: the method exists but its trait bounds are not satisfied).
    ///
    /// Conservative on both halves: the param must base-resolve to a recorded
    /// clone-bound record (never a built-in collection or a non-generic record),
    /// AND the body must contain a `MethodCall` (driving the record) — a fn that
    /// merely receives such a record but never calls a method on it emits no
    /// bound-requiring code and is left un-constrained.
    fn params_drive_clone_bound_record(&self, params: &[AIRNode], body: &AIRNode) -> bool {
        let takes_clone_bound_record = params.iter().any(|p| {
            let NodeKind::Param { ty: Some(t), .. } = &p.kind else {
                return false;
            };
            self.clone_bound_records
                .contains(&self.type_expr_base_name(t))
        });
        if !takes_clone_bound_record {
            return false;
        }
        struct MethodCallScan {
            found: bool,
        }
        impl bock_air::visitor::Visitor for MethodCallScan {
            fn visit_node(&mut self, node: &AIRNode) {
                if self.found {
                    return;
                }
                // A user method call (`cur.next()`) lowers to a `Call` whose
                // callee is a `FieldAccess` (the lowerer's desugared-self-call
                // shape — see `generator::desugared_self_call`), not a
                // `MethodCall` node; the bare `MethodCall` variant never reaches
                // codegen for these. Treat either form as "drives a method".
                let is_call_on_member = matches!(&node.kind,
                    NodeKind::Call { callee, .. }
                        if matches!(callee.kind, NodeKind::FieldAccess { .. }));
                if is_call_on_member || matches!(node.kind, NodeKind::MethodCall { .. }) {
                    self.found = true;
                    return;
                }
                bock_air::visitor::walk_node(self, node);
            }
        }
        let mut scan = MethodCallScan { found: false };
        bock_air::visitor::Visitor::visit_node(&mut scan, body);
        scan.found
    }

    /// The `Enum::` qualifier for a variant *path* if its last segment is a
    /// registered user enum variant, else `None`. The built-in
    /// `Optional`/`Result` pre-seeds are intentionally excluded here: their
    /// constructions and patterns are handled by the bespoke Rust lowering
    /// (`Some(x)`/`None`/`Ok`/`Err` map to `std::option`/`std::result`), which
    /// must not be rewritten to `Optional::Some`.
    fn variant_enum_qualifier(&self, path: &bock_ast::TypePath) -> Option<String> {
        let info = crate::generator::registered_variant(&self.enum_variants, path)?;
        if matches!(info.enum_name.as_str(), "Optional" | "Result") {
            return None;
        }
        Some(info.enum_name.clone())
    }

    /// As [`Self::variant_enum_qualifier`] but for a bare identifier name (a
    /// unit-variant construction lowers to `Identifier`, or a tuple-variant
    /// construction's callee is an `Identifier`).
    fn variant_enum_qualifier_for_name(&self, name: &str) -> Option<String> {
        let info = self.enum_variants.get(name)?;
        if matches!(info.enum_name.as_str(), "Optional" | "Result") {
            return None;
        }
        Some(info.enum_name.clone())
    }

    /// True when the real `core.compare.Ordering` enum is reachable in this
    /// program (its `Less` variant is a registered user enum variant). When
    /// `core.compare` is `use`d, the actual `enum Ordering` decl is emitted; the
    /// `Less`/`Equal`/`Greater` references and match patterns must then use that
    /// user enum (`Ordering::Less`), not the `std::cmp::Ordering` bridge the
    /// prelude form uses when the enum is *not* reachable (e.g. a bare primitive
    /// `compare`).
    fn ordering_enum_reachable(&self) -> bool {
        self.enum_variants
            .get("Less")
            .is_some_and(|info| info.enum_name == "Ordering")
    }

    fn finish(mut self) -> String {
        if self.buf.is_empty() {
            return self.buf;
        }
        // rustfmt wraps an inner-attribute list of this many items across lines
        // (regardless of the line fitting in `max_width`), so emit the wrapped
        // form directly — the §20.6.2 codegen-formatter agreement requires the
        // output to pass `rustfmt --check` cleanly on first generation (S7).
        let mut prefix = String::from(
            "#![allow(\n    unused_variables,\n    unused_imports,\n    unused_parens,\n    dead_code,\n    non_upper_case_globals\n)]\n\n",
        );
        if self.needs_rc_import {
            prefix.push_str("use std::rc::Rc;\n");
        }
        if self.needs_arc_import {
            prefix.push_str("use std::sync::Arc;\n");
        }
        if !prefix.ends_with("\n\n") {
            prefix.push('\n');
        }
        self.buf.insert_str(0, &prefix);
        self.buf
    }

    /// Finish one file of the per-module native tree (S3): prepend the per-file
    /// `#![allow(...)]` inner attribute and any `use std::{rc,sync}` the body
    /// needs, then return the buffer. The cross-module `use crate::<m>::<x>;`
    /// statements (and the shared-runtime `use`) are emitted into the buffer by
    /// the `Module` arm, so they already sit at the top of the body — this only
    /// adds the crate/std-level preamble. `#![allow(...)]` is a module-level
    /// inner attribute valid at the head of any module file (the crate root
    /// `main.rs` *and* a submodule like `src/core/option.rs`).
    fn finish_per_module(mut self) -> String {
        if self.buf.is_empty() {
            return self.buf;
        }
        // rustfmt wraps an inner-attribute list of this many items across lines
        // (regardless of the line fitting in `max_width`), so emit the wrapped
        // form directly — the §20.6.2 codegen-formatter agreement requires the
        // output to pass `rustfmt --check` cleanly on first generation (S7).
        let mut prefix = String::from(
            "#![allow(\n    unused_variables,\n    unused_imports,\n    unused_parens,\n    dead_code,\n    non_upper_case_globals\n)]\n\n",
        );
        if self.needs_rc_import {
            prefix.push_str("use std::rc::Rc;\n");
        }
        if self.needs_arc_import {
            prefix.push_str("use std::sync::Arc;\n");
        }
        if !prefix.ends_with("\n\n") {
            prefix.push('\n');
        }
        self.buf.insert_str(0, &prefix);
        self.buf
    }

    /// Pre-seed the effect registries (`effect_ops`, `composite_effects`) from
    /// every module's top-level `EffectDecl`s. In the per-module path each
    /// module is emitted by its own forked context, so a bare op `log(...)` used
    /// in `main` whose effect `Log` is declared in another module would not be
    /// recognised as an effect op (and not rewritten to `__handler.log(...)`)
    /// without pre-seeding from the whole reachable set. Mirrors how
    /// `enum_variants` / `trait_decls` are collected across the reached modules
    /// and the Python / JS / TS backends' equivalents.
    fn seed_effect_registries(&mut self, modules: &[(&AIRModule, &std::path::Path)]) {
        for (module, _) in modules {
            let NodeKind::Module { items, .. } = &module.kind else {
                continue;
            };
            for item in items {
                let NodeKind::EffectDecl {
                    name,
                    components,
                    operations,
                    ..
                } = &item.kind
                else {
                    continue;
                };
                if !components.is_empty() {
                    let comp_names: Vec<String> = components
                        .iter()
                        .map(|tp| {
                            tp.segments
                                .last()
                                .map_or("effect".to_string(), |s| s.name.clone())
                        })
                        .collect();
                    self.composite_effects.insert(name.name.clone(), comp_names);
                    continue;
                }
                for op in operations {
                    if let NodeKind::FnDecl { name: op_name, .. } = &op.kind {
                        self.effect_ops
                            .insert(op_name.name.clone(), name.name.clone());
                    }
                }
            }
        }
    }

    /// Emit the per-module cross-module `use crate::<m>::<x>;` statements at the
    /// top of the file: the explicit `use`d symbols and the implicit
    /// §18.2-prelude names this module references but does not `use`. Grouped
    /// one `use crate::<path>::{a, b};` per source module, deterministically
    /// ordered. The dotted declared path `core.option` becomes the crate path
    /// `crate::core::option`.
    ///
    /// Built-in prelude *value/type* names that lower to native Rust
    /// (`Optional`/`Result` → `Option`/`Result`, `Some`/`None`/`Ok`/`Err`) are
    /// skipped — they are not real exports of the declaring stdlib module, so a
    /// `use crate::core::option::Some;` would not resolve. Cross-module
    /// references to those resolve through the native lowering instead.
    fn emit_cross_module_uses(&mut self, imports: &[AIRNode]) {
        use std::collections::BTreeMap;
        // crate-path → set of leaf symbol names (sorted, deduped on render).
        let mut by_module: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();

        // Explicit `use mod.{a, b}` imports.
        for import in imports {
            let NodeKind::ImportDecl { path, items } = &import.kind else {
                continue;
            };
            let dotted = path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if dotted.is_empty() {
                continue;
            }
            if let bock_ast::ImportItems::Named(named) = items {
                for n in named {
                    if RS_NATIVE_PRELUDE_NAMES.contains(&n.name.name.as_str()) {
                        continue;
                    }
                    by_module
                        .entry(dotted.clone())
                        .or_default()
                        .insert(n.name.name.clone());
                }
            }
            // `use Foo` / `use Foo.*`: the referenced names are resolved as
            // implicit imports below, so no statement is needed for the bare
            // module/glob form.
        }

        // Implicit imports: prelude-visible names referenced but not `use`d.
        for (module_path, name) in &self.implicit_imports {
            if RS_NATIVE_PRELUDE_NAMES.contains(&name.as_str()) {
                continue;
            }
            by_module
                .entry(module_path.clone())
                .or_default()
                .insert(name.clone());
        }

        for (dotted, names) in by_module {
            if names.is_empty() {
                continue;
            }
            let crate_path = format!("crate::{}", dotted.replace('.', "::"));
            let joined = names.into_iter().collect::<Vec<_>>().join(", ");
            self.writeln(&format!("use {crate_path}::{{{joined}}};"));
        }
    }

    fn indent_str(&self) -> String {
        "    ".repeat(self.indent)
    }

    fn write_indent(&mut self) {
        let indent = self.indent_str();
        self.buf.push_str(&indent);
    }

    fn writeln(&mut self, s: &str) {
        self.write_indent();
        self.buf.push_str(s);
        self.buf.push('\n');
    }

    // ── Prelude function mapping ──────────────────────────────────────────

    /// Emit an expression into a temporary buffer and return the string.
    fn expr_to_string(&mut self, node: &AIRNode) -> Result<String, CodegenError> {
        let start = self.buf.len();
        self.emit_expr(node)?;
        let s = self.buf[start..].to_string();
        self.buf.truncate(start);
        Ok(s)
    }

    /// Map Bock prelude functions to Rust equivalents.
    fn map_prelude_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<Option<String>, CodegenError> {
        let name = match &callee.kind {
            NodeKind::Identifier { name } => name.name.as_str(),
            _ => return Ok(None),
        };
        let arg_strs: Vec<String> = args
            .iter()
            .map(|a| self.expr_to_string(&a.value))
            .collect::<Result<_, _>>()?;
        let code = match name {
            "println" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("println!(\"{{}}\", {a})")
            }
            "print" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("print!(\"{{}}\", {a})")
            }
            "debug" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("dbg!(&{a})")
            }
            "assert" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("assert!({a})")
            }
            "todo" => "todo!()".to_string(),
            "unreachable" => "unreachable!()".to_string(),
            "sleep" => {
                let a = arg_strs.first().map_or(String::new(), |s| s.clone());
                format!("tokio::time::sleep(std::time::Duration::from_nanos(({a}) as u64))")
            }
            _ => return Ok(None),
        };
        Ok(Some(code))
    }

    /// Emit a built-in `Optional`/`Result` method call to its Rust form.
    ///
    /// Bock `Optional[T]`/`Result[T, E]` lower to Rust's native `Option<T>` /
    /// `Result<T, E>`, and the built-in methods are (nearly) the native methods,
    /// so this is mostly a name passthrough — *except* it (a) clones the receiver
    /// for the by-value (consuming) methods (`unwrap`/`unwrap_or`/`map`/…) so a
    /// later `r.is_ok()` does not hit a borrow-of-moved-value error when the same
    /// value is read again, and (b) renames `flat_map` → the native `and_then`.
    /// `T: Clone` holds for the v1 payload types (Int/Float/String/Bool/nested
    /// Option/Result). Recognised via the checker's `recv_kind` annotation.
    /// Returns `true` if handled.
    fn try_emit_container_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let resolved = crate::generator::desugared_optional_method(node, callee, args)
            .or_else(|| crate::generator::desugared_result_method(node, callee, args));
        let Some((recv, method, rest)) = resolved else {
            return Ok(false);
        };
        // `is_*` take `&self` (no move); the rest consume `self`, so clone the
        // receiver to keep it usable afterwards.
        let consuming = !matches!(method, "is_some" | "is_none" | "is_ok" | "is_err");
        let native = match method {
            "flat_map" => "and_then",
            other => other,
        };
        self.buf.push('(');
        self.emit_expr(recv)?;
        self.buf.push(')');
        if consuming {
            self.buf.push_str(".clone()");
        }
        let _ = write!(self.buf, ".{native}(");
        for (i, arg) in rest.iter().enumerate() {
            if i > 0 {
                self.buf.push_str(", ");
            }
            self.emit_expr(&arg.value)?;
        }
        self.buf.push(')');
        Ok(true)
    }

    /// Emit a read-only `List` built-in method call to its Rust form.
    ///
    /// Lists are `Vec<T>`. `len`/`length`/`count` coerce `usize` → `i64`
    /// (`(r.len() as i64)`) so the result composes with Bock's `Int`.
    /// `Optional`-returning methods use Rust's *native* `Option<T>` (the rep the
    /// Rust `match` lowering already expects): `get` is `r.get(i as
    /// usize).cloned()`, `first`/`last` are `r.first()/last().cloned()`, and
    /// `index_of` maps the found position to `i64`. `.cloned()` requires
    /// `T: Clone`, which the v1 element types (Int/Float/String/Bool) satisfy.
    /// `concat` is a functional clone-and-extend.
    fn try_emit_list_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) =
            crate::generator::desugared_list_method(node, callee, args)
        else {
            return Ok(false);
        };
        let recv_str = self.expr_to_string(recv)?;
        let code = match method {
            "len" | "length" | "count" => format!("(({recv_str}).len() as i64)"),
            "is_empty" => format!("({recv_str}).is_empty()"),
            "get" => {
                let Some(idx) = rest.first() else {
                    return Ok(false);
                };
                let i = self.expr_to_string(&idx.value)?;
                format!("({recv_str}).get(({i}) as usize).cloned()")
            }
            "first" => format!("({recv_str}).first().cloned()"),
            "last" => format!("({recv_str}).last().cloned()"),
            "contains" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                let x = self.expr_to_string(&x.value)?;
                format!("({recv_str}).contains(&({x}))")
            }
            "index_of" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                let x = self.expr_to_string(&x.value)?;
                format!("({recv_str}).iter().position(|__e| __e == &({x})).map(|__i| __i as i64)")
            }
            "concat" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "{{ let mut __v = ({recv_str}).clone(); __v.extend(({o}).iter().cloned()); __v }}"
                )
            }
            "join" => {
                let Some(sep) = rest.first() else {
                    return Ok(false);
                };
                let sep = self.expr_to_string(&sep.value)?;
                format!("({recv_str}).join(({sep}).as_str())")
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit a functional (closure-taking) `List` built-in method call to its
    /// Rust form.
    ///
    /// Recognised via [`crate::generator::desugared_list_functional_method`].
    /// The receiver is `.clone()`d (Bock lists have value semantics — the
    /// receiver var stays usable) and iterated by value: `map`/`flat_map`/`fold`/
    /// `for_each`/`reduce` drive `.clone().into_iter()`; the predicate combinators
    /// `filter`/`find`/`any`/`all` drive `.iter().cloned()` and adapt their `&T`
    /// item to the Bock closure's by-value `T` parameter via `__x.clone()`. The
    /// closure is captured *once* into `__f` so it is evaluated a single time
    /// (the desugared `recv.map(recv, cb)` shape the generic fall-through emits
    /// otherwise fails with `no method 'map' found for Vec`). `T: Clone` holds for
    /// every v1 element type (Int/Float/String/Bool and `#[derive(Clone)]`
    /// records); a generic-`List[T]` use is gated to synthesize the bound via
    /// [`Self::body_clones_collection_element`].
    fn try_emit_list_functional_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) =
            crate::generator::desugared_list_functional_method(node, callee, args)
        else {
            return Ok(false);
        };
        let recv_str = self.expr_to_string(recv)?;
        // The closure is emitted *inline* into the adapter rather than bound to a
        // `let __f` first: a closure stored in a `let` whose params are the
        // inferred-placeholder `|x: _|` cannot always back-infer its parameter
        // types from a later `into_iter().map(__f)` (`E0282`), whereas a closure
        // passed directly into the iterator adapter takes its parameter type from
        // the adapter's item type.
        let code = match method {
            "map" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&cb.value)?;
                format!("({recv_str}).clone().into_iter().map({f}).collect::<Vec<_>>()")
            }
            "flat_map" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&cb.value)?;
                format!("({recv_str}).clone().into_iter().flat_map({f}).collect::<Vec<_>>()")
            }
            "filter" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&cb.value)?;
                // The Bock predicate takes `T` by value, but `Iterator::filter`
                // passes `&T` — and a closure literal cannot infer its parameter
                // type from an immediate application. So the predicate is flowed
                // *directly* into `.map(..)` (which pins its `T` param) to compute
                // a parallel `Vec<bool>`, then zipped with the elements and
                // filtered on the bool, then projected back to the elements.
                format!(
                    "{{ let __p: Vec<bool> = ({recv_str}).clone().into_iter().map({f}).collect(); \
                     ({recv_str}).iter().cloned().zip(__p).filter(|__t| __t.1).map(|__t| __t.0).collect::<Vec<_>>() }}"
                )
            }
            "find" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&cb.value)?;
                // Same map-pinning approach as `filter`; `find` then returns the
                // first element whose paired predicate is true (`Option<T>`, the
                // Rust Optional rep).
                format!(
                    "{{ let __p: Vec<bool> = ({recv_str}).clone().into_iter().map({f}).collect(); \
                     ({recv_str}).iter().cloned().zip(__p).find(|__t| __t.1).map(|__t| __t.0) }}"
                )
            }
            "any" | "all" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&cb.value)?;
                // Compute the predicate over each element via `.map(..)` (which
                // pins the closure's `T` param), then short-circuit with the
                // bool-iterator `any`/`all`.
                format!("({recv_str}).clone().into_iter().map({f}).{method}(|__b| __b)")
            }
            "reduce" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&cb.value)?;
                // Bock `reduce` has no seed (first element is the accumulator) and
                // returns `T`; `Iterator::reduce` returns `Option<T>`.
                format!(
                    "({recv_str}).clone().into_iter().reduce({f}).expect(\"reduce on an empty list\")"
                )
            }
            "fold" => {
                let (Some(init), Some(cb)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                let init = self.expr_to_string(&init.value)?;
                let f = self.expr_to_string(&cb.value)?;
                format!("({recv_str}).clone().into_iter().fold({init}, {f})")
            }
            "for_each" => {
                let Some(cb) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&cb.value)?;
                format!("({recv_str}).clone().into_iter().for_each({f})")
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit a built-in `Map[K, V]` method call to its Rust form (native
    /// `std::collections::HashMap`).
    ///
    /// Recognised via [`crate::generator::desugared_map_method`] (gated on
    /// `recv_kind = "Map"`) and wired *before* [`Self::try_emit_list_method`],
    /// so a `Map` receiver's `get`/`contains_key`/`len` no longer route through
    /// the `List` path (where `get` cast the *key* to `usize` and indexed the
    /// map as a slice). `get` returns Rust's native `Option<V>` (`.get(&k)
    /// .cloned()`), the same rep the Rust `match` / Optional lowering expects.
    /// Mutating methods (`set`/`delete`/`merge`) clone-then-mutate and return
    /// the new map (Bock map value semantics; the receiver var need not be
    /// `mut`). `K: Hash + Eq` and `K, V: Clone` hold for the v1 element types.
    /// Returns `true` if handled.
    fn try_emit_map_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) = crate::generator::desugared_map_method(node, callee, args)
        else {
            return Ok(false);
        };
        let recv_str = self.expr_to_string(recv)?;
        let code = match method {
            "len" | "length" | "count" => format!("(({recv_str}).len() as i64)"),
            "is_empty" => format!("({recv_str}).is_empty()"),
            "contains_key" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                let k = self.expr_to_string(&k.value)?;
                format!("({recv_str}).contains_key(&({k}))")
            }
            "get" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                let k = self.expr_to_string(&k.value)?;
                format!("({recv_str}).get(&({k})).cloned()")
            }
            "set" => {
                let (Some(k), Some(v)) = (rest.first(), rest.get(1)) else {
                    return Ok(false);
                };
                let k = self.expr_to_string(&k.value)?;
                let v = self.expr_to_string(&v.value)?;
                format!("{{ let mut __m = ({recv_str}).clone(); __m.insert({k}, {v}); __m }}")
            }
            "delete" => {
                let Some(k) = rest.first() else {
                    return Ok(false);
                };
                let k = self.expr_to_string(&k.value)?;
                format!("{{ let mut __m = ({recv_str}).clone(); __m.remove(&({k})); __m }}")
            }
            "merge" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!("{{ let mut __m = ({recv_str}).clone(); __m.extend(({o}).clone()); __m }}")
            }
            "filter" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "({recv_str}).iter().filter(|(__k, __v)| ({f})((*__k).clone(), (*__v).clone()))\
                     .map(|(__k, __v)| (__k.clone(), __v.clone()))\
                     .collect::<std::collections::HashMap<_, _>>()"
                )
            }
            "keys" => {
                format!("({recv_str}).keys().cloned().collect::<Vec<_>>()")
            }
            "values" => {
                format!("({recv_str}).values().cloned().collect::<Vec<_>>()")
            }
            "entries" | "to_list" => {
                format!(
                    "({recv_str}).iter().map(|(__k, __v)| (__k.clone(), __v.clone()))\
                     .collect::<Vec<_>>()"
                )
            }
            "for_each" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "{{ for (__k, __v) in ({recv_str}).iter() {{ \
                     ({f})(__k.clone(), __v.clone()); }} }}"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Emit a built-in `Set[E]` method call to its Rust form (native
    /// `std::collections::HashSet`).
    ///
    /// Recognised via [`crate::generator::desugared_set_method`] (gated on
    /// `recv_kind = "Set"`) and wired *before* [`Self::try_emit_list_method`].
    /// Set algebra maps to the native `HashSet` combinators; `contains` is the
    /// native membership test (not the `List` linear scan). Mutating methods
    /// (`add`/`remove`) clone-then-mutate and return the new set. `E: Hash + Eq
    /// + Clone` holds for the v1 element types.
    fn try_emit_set_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest)) = crate::generator::desugared_set_method(node, callee, args)
        else {
            return Ok(false);
        };
        let recv_str = self.expr_to_string(recv)?;
        let code = match method {
            "len" | "length" | "count" => format!("(({recv_str}).len() as i64)"),
            "is_empty" => format!("({recv_str}).is_empty()"),
            "contains" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                let x = self.expr_to_string(&x.value)?;
                format!("({recv_str}).contains(&({x}))")
            }
            "add" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                let x = self.expr_to_string(&x.value)?;
                format!("{{ let mut __s = ({recv_str}).clone(); __s.insert({x}); __s }}")
            }
            "remove" => {
                let Some(x) = rest.first() else {
                    return Ok(false);
                };
                let x = self.expr_to_string(&x.value)?;
                format!("{{ let mut __s = ({recv_str}).clone(); __s.remove(&({x})); __s }}")
            }
            "union" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "({recv_str}).union(&({o})).cloned().collect::<std::collections::HashSet<_>>()"
                )
            }
            "intersection" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "({recv_str}).intersection(&({o})).cloned()\
                     .collect::<std::collections::HashSet<_>>()"
                )
            }
            "difference" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!(
                    "({recv_str}).difference(&({o})).cloned()\
                     .collect::<std::collections::HashSet<_>>()"
                )
            }
            "is_subset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!("({recv_str}).is_subset(&({o}))")
            }
            "is_superset" => {
                let Some(o) = rest.first() else {
                    return Ok(false);
                };
                let o = self.expr_to_string(&o.value)?;
                format!("({recv_str}).is_superset(&({o}))")
            }
            "filter" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "({recv_str}).iter().filter(|__x| ({f})((*__x).clone())).cloned()\
                     .collect::<std::collections::HashSet<_>>()"
                )
            }
            "map" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!(
                    "({recv_str}).iter().map(|__x| ({f})(__x.clone()))\
                     .collect::<std::collections::HashSet<_>>()"
                )
            }
            "to_list" => {
                format!("({recv_str}).iter().cloned().collect::<Vec<_>>()")
            }
            "for_each" => {
                let Some(f) = rest.first() else {
                    return Ok(false);
                };
                let f = self.expr_to_string(&f.value)?;
                format!("{{ for __x in ({recv_str}).iter() {{ ({f})(__x.clone()); }} }}")
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Lower a primitive trait-bridge method call (`compare`/`eq`/`to_string`/
    /// `display` on a primitive receiver) to its Rust intrinsic.
    ///
    /// `(1).compare(2)` resolves in the checker to `Ordering`, but `i64` has no
    /// `.compare` method; this maps it to `i64::cmp` (→ `std::cmp::Ordering`,
    /// which the construction/match sides also use). `compare` on a float maps
    /// to `partial_cmp(...).unwrap()` (floats are only `PartialOrd`). `eq`
    /// becomes `==`; `to_string`/`display` become `.to_string()`.
    /// Best-effort detection that `node` evaluates to a Rust `String` (or
    /// `&str`), used to route `+` to `format!` concat. Recognises the syntactic
    /// shapes that unambiguously produce a string: a string literal, a
    /// `format!`-lowered interpolation, and the desugared `String` built-in
    /// methods whose return type is `String` (`to_upper`/`to_lower`/`trim`/
    /// `replace`) or the `to_string`/`display` bridge. This is intentionally
    /// conservative — a false negative leaves a non-string `+` untouched (still
    /// correct for numbers); the recognised shapes cover the string-concat code
    /// that arises in practice. A nested `+` whose own operands are strings is
    /// itself a `String`, so the recursion threads through chained concat.
    fn expr_is_string_rs(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::Literal {
                lit: Literal::String(_),
            } => true,
            NodeKind::Interpolation { .. } => true,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left,
                right,
            } => Self::expr_is_string_rs(left) || Self::expr_is_string_rs(right),
            NodeKind::Call { callee, .. } => {
                let NodeKind::FieldAccess { field, .. } = &callee.kind else {
                    return false;
                };
                matches!(
                    field.name.as_str(),
                    "to_upper" | "to_lower" | "trim" | "replace" | "to_string" | "display"
                )
            }
            _ => false,
        }
    }

    /// Lower a desugared `String` built-in method call (`recv_kind =
    /// "Primitive:String"`) to its native Rust string op. Wired into the `Call`
    /// arm *before* `try_emit_list_method` so a String receiver's
    /// `len`/`contains`/`is_empty` dispatch here, not through the List path.
    ///
    /// `len` is the Unicode SCALAR count (`(s).chars().count() as i64`) per spec
    /// §18.3 — Rust's `str::len` is the BYTE length, so `byte_len` maps to it
    /// (`(s).len() as i64`). String args (literals emit as owned `String`) are
    /// passed by reference (`&(..)`), which derefs to the `&str`/`Pattern` the
    /// `str` methods expect. `replace` replaces ALL occurrences (Rust's default).
    /// `split` collects to a `Vec<String>`, the List runtime rep.
    fn try_emit_string_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        // Gate on the checker's `recv_kind = "Primitive:String"` stamp directly
        // (rather than [`crate::generator::desugared_string_method`], which only
        // admits the cross-backend `STRING_METHODS` subset). Rust can lower a
        // wider set — `slice`/`substring`/`char_at`/`index_of`/`repeat`/`reverse`
        // — to native `str` ops, so it recognises the full resolved String method
        // surface here without widening the shared const (which would force the
        // other backends to handle the extra names too).
        if crate::generator::primitive_recv_kind(node) != Some("String") {
            return Ok(false);
        }
        let Some((recv, field, rest)) = crate::generator::desugared_self_call(callee, args) else {
            return Ok(false);
        };
        let method = field.name.as_str();
        let recv_str = self.expr_to_string(recv)?;
        let arg0 = |this: &mut Self| -> Result<Option<String>, CodegenError> {
            rest.first()
                .map(|a| this.expr_to_string(&a.value))
                .transpose()
        };
        let code = match method {
            "len" | "length" | "count" => format!("(({recv_str}).chars().count() as i64)"),
            "byte_len" => format!("(({recv_str}).len() as i64)"),
            "is_empty" => format!("({recv_str}).is_empty()"),
            "to_upper" => format!("({recv_str}).to_uppercase()"),
            "to_lower" => format!("({recv_str}).to_lowercase()"),
            "trim" => format!("({recv_str}).trim().to_string()"),
            "trim_start" => format!("({recv_str}).trim_start().to_string()"),
            "trim_end" => format!("({recv_str}).trim_end().to_string()"),
            "reverse" => format!("({recv_str}).chars().rev().collect::<String>()"),
            "to_string" | "display" => format!("({recv_str}).to_string()"),
            "repeat" => {
                let Some(n) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).repeat(({n}) as usize)")
            }
            "contains" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).contains(&({p}) as &str)")
            }
            "starts_with" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).starts_with(&({p}) as &str)")
            }
            "ends_with" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).ends_with(&({p}) as &str)")
            }
            "replace" => {
                let Some(from) = arg0(self)? else {
                    return Ok(false);
                };
                let Some(to) = rest
                    .get(1)
                    .map(|a| self.expr_to_string(&a.value))
                    .transpose()?
                else {
                    return Ok(false);
                };
                format!("({recv_str}).replace(&({from}) as &str, &({to}) as &str)")
            }
            "split" => {
                let Some(sep) = arg0(self)? else {
                    return Ok(false);
                };
                format!(
                    "({recv_str}).split(&({sep}) as &str).map(|__p| __p.to_string()).collect::<Vec<String>>()"
                )
            }
            // `slice`/`substring(start, end)` are scalar-index half-open
            // substrings (spec §18.3 — `len` is the Unicode scalar count, so
            // indices are scalar positions, not bytes). Lowered via a char
            // iterator so multibyte input is handled correctly and the result is
            // an owned `String`. `start`/`end` are clamped by `take`'s saturating
            // subtraction (`end.saturating_sub(start)`).
            "slice" | "substring" => {
                let Some(start) = arg0(self)? else {
                    return Ok(false);
                };
                let Some(end) = rest
                    .get(1)
                    .map(|a| self.expr_to_string(&a.value))
                    .transpose()?
                else {
                    return Ok(false);
                };
                format!(
                    "({recv_str}).chars().skip(({start}) as usize).take((({end}) as i64 - ({start}) as i64).max(0) as usize).collect::<String>()"
                )
            }
            // `char_at(i)` returns `Optional[Char]` — `None` when out of range.
            "char_at" => {
                let Some(i) = arg0(self)? else {
                    return Ok(false);
                };
                format!("({recv_str}).chars().nth(({i}) as usize)")
            }
            // `index_of(needle)` returns `Optional[Int]` — the scalar index of the
            // first match, or `None`. Rust's `str::find` yields a *byte* offset,
            // so convert it to a scalar index via the char-boundary count.
            "index_of" => {
                let Some(p) = arg0(self)? else {
                    return Ok(false);
                };
                format!(
                    "({recv_str}).find(&({p}) as &str).map(|__b| ({recv_str})[..__b].chars().count() as i64)"
                )
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Lower a desugared numeric/`Char`/`Bool` primitive method (`recv_kind =
    /// "Primitive:Int" | "Primitive:Float" | "Primitive:Char" | "Primitive:Bool"`)
    /// to its native Rust form. Covers the conversion and math methods the checker
    /// resolves on the scalar primitives — `to_float`/`to_int`/`abs`/`min`/`max`/
    /// `clamp`/`floor`/`ceil`/`round`/`sqrt`/… — none of which exist as inherent
    /// methods on `i64`/`f64` in Rust. Wired into the `Call` arm alongside
    /// [`Self::try_emit_string_method`], before the generic desugared-self-call
    /// fall-through (which would emit `n.to_float(n)`, undefined on `i64`).
    /// `compare`/`eq`/`to_string`/`display` stay on the primitive *bridge* path.
    fn try_emit_numeric_method(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let prim = match crate::generator::primitive_recv_kind(node) {
            Some(p @ ("Int" | "Float" | "Char" | "Bool")) => p,
            _ => return Ok(false),
        };
        let Some((recv, field, rest)) = crate::generator::desugared_self_call(callee, args) else {
            return Ok(false);
        };
        let method = field.name.as_str();
        let recv_str = self.expr_to_string(recv)?;
        let arg = |this: &mut Self, i: usize| -> Result<Option<String>, CodegenError> {
            rest.get(i)
                .map(|a| this.expr_to_string(&a.value))
                .transpose()
        };
        let code = match (prim, method) {
            // Int → Float / Float → Int conversions.
            ("Int", "to_float") => format!("(({recv_str}) as f64)"),
            ("Float", "to_int") => format!("(({recv_str}) as i64)"),
            // `Char.to_int` is the scalar value; `Bool.to_int` is 0/1.
            ("Char", "to_int") => format!("(({recv_str}) as i64)"),
            ("Bool", "to_int") => format!("(if ({recv_str}) {{ 1i64 }} else {{ 0i64 }})"),
            // Int math.
            ("Int", "abs") => format!("({recv_str}).abs()"),
            ("Int", "min") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("({recv_str}).min({o})")
            }
            ("Int", "max") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("({recv_str}).max({o})")
            }
            ("Int", "clamp") => {
                let (Some(lo), Some(hi)) = (arg(self, 0)?, arg(self, 1)?) else {
                    return Ok(false);
                };
                format!("({recv_str}).clamp({lo}, {hi})")
            }
            ("Int", "shift_left") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("(({recv_str}) << ({o}))")
            }
            ("Int", "shift_right") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("(({recv_str}) >> ({o}))")
            }
            // Float math.
            ("Float", "abs") => format!("({recv_str}).abs()"),
            ("Float", "floor") => format!("({recv_str}).floor()"),
            ("Float", "ceil") => format!("({recv_str}).ceil()"),
            ("Float", "round") => format!("({recv_str}).round()"),
            ("Float", "sqrt") => format!("({recv_str}).sqrt()"),
            ("Float", "is_nan") => format!("({recv_str}).is_nan()"),
            ("Float", "is_infinite") => format!("({recv_str}).is_infinite()"),
            ("Float", "min") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("({recv_str}).min({o})")
            }
            ("Float", "max") => {
                let Some(o) = arg(self, 0)? else {
                    return Ok(false);
                };
                format!("({recv_str}).max({o})")
            }
            ("Float", "clamp") => {
                let (Some(lo), Some(hi)) = (arg(self, 0)?, arg(self, 1)?) else {
                    return Ok(false);
                };
                format!("({recv_str}).clamp({lo}, {hi})")
            }
            // Bool.
            ("Bool", "negate") => format!("(!({recv_str}))"),
            // Char.
            ("Char", "to_upper") => {
                format!("({recv_str}).to_uppercase().next().unwrap_or({recv_str})")
            }
            ("Char", "to_lower") => {
                format!("({recv_str}).to_lowercase().next().unwrap_or({recv_str})")
            }
            ("Char", "is_alpha") => format!("({recv_str}).is_alphabetic()"),
            ("Char", "is_digit") => format!("({recv_str}).is_ascii_digit()"),
            ("Char", "is_whitespace") => format!("({recv_str}).is_whitespace()"),
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    fn try_emit_primitive_bridge(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest, prim)) =
            crate::generator::primitive_bridge_call(node, callee, args)
        else {
            return Ok(false);
        };
        // Floats are only `PartialOrd` in Rust; everything else is `Ord`.
        let partial = prim.starts_with("Float") || prim == "BigFloat" || prim == "Decimal";
        self.emit_bridge_method(recv, method, rest, partial)
    }

    /// Lower a sealed-core-trait bridge method on a *bounded generic type
    /// variable* (`a.eq(b)` / `a.compare(b)` / `a.to_string()` inside
    /// `eq_check[T: Equatable]`) to its Rust intrinsic. The generic analogue of
    /// [`Self::try_emit_primitive_bridge`] (GAP-C): the receiver is `T`, whose
    /// sealed-core bound is rewritten to the matching std trait
    /// (`Equatable`→`PartialEq`, `Comparable`→`Ord`, `Displayable`→`Display`) at
    /// the signature, so `==` / `.cmp(&…)` / `.to_string()` type-check. Only fires
    /// when the bound trait is sealed-core and NOT a user-declared trait (a user
    /// trait provides the method through its own `impl`).
    fn try_emit_trait_bound_bridge(
        &mut self,
        node: &AIRNode,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let Some((recv, method, rest, _tr)) =
            crate::generator::trait_bound_bridge_call(node, callee, args, &self.trait_decls)
        else {
            return Ok(false);
        };
        // A generic `T: Ord` always uses the total-order `.cmp`; there is no
        // partial-order generic bound (a `Float`-only bound is not expressible).
        self.emit_bridge_method(recv, method, rest, false)
    }

    /// Shared body of the primitive / trait-bound bridges: emit the native Rust
    /// form of `compare` (`Ordering` via `.cmp`/`.partial_cmp`), `eq` (`==`), or
    /// `to_string`/`display` (`.to_string()`) for the receiver + remaining args.
    /// `partial` selects `.partial_cmp(..).unwrap()` over `.cmp(..)` for the
    /// `PartialOrd`-only float types.
    fn emit_bridge_method(
        &mut self,
        recv: &AIRNode,
        method: &str,
        rest: &[bock_air::AirArg],
        partial: bool,
    ) -> Result<bool, CodegenError> {
        let recv_str = self.expr_to_string(recv)?;
        let code = match method {
            "compare" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                if partial {
                    format!("({recv_str}).partial_cmp(&({other})).unwrap()")
                } else {
                    format!("({recv_str}).cmp(&({other}))")
                }
            }
            "eq" => {
                let Some(other) = rest.first() else {
                    return Ok(false);
                };
                let other = self.expr_to_string(&other.value)?;
                format!("(({recv_str}) == ({other}))")
            }
            "to_string" | "display" => format!("({recv_str}).to_string()"),
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Recognise `Duration.xxx(...)` / `Instant.xxx(...)` associated-function
    /// calls and emit equivalent Rust `std::time` usage. Durations are i64
    /// nanoseconds; Instants are `std::time::Instant`.
    fn try_emit_time_assoc_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let NodeKind::FieldAccess { object, field } = &callee.kind else {
            return Ok(false);
        };
        let NodeKind::Identifier { name: type_name } = &object.kind else {
            return Ok(false);
        };
        let arg_strs: Vec<String> = args
            .iter()
            .map(|a| self.expr_to_string(&a.value))
            .collect::<Result<_, _>>()?;
        let arg0 = || arg_strs.first().cloned().unwrap_or_default();
        let code = match (type_name.name.as_str(), field.name.as_str()) {
            ("Duration", "zero") => "0i64".to_string(),
            ("Duration", "nanos") => format!("(({}) as i64)", arg0()),
            ("Duration", "micros") => format!("((({}) as i64) * 1_000)", arg0()),
            ("Duration", "millis") => format!("((({}) as i64) * 1_000_000)", arg0()),
            ("Duration", "seconds") => format!("((({}) as i64) * 1_000_000_000)", arg0()),
            ("Duration", "minutes") => format!("((({}) as i64) * 60_000_000_000)", arg0()),
            ("Duration", "hours") => format!("((({}) as i64) * 3_600_000_000_000)", arg0()),
            ("Instant", "now") => "std::time::Instant::now()".to_string(),
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    /// Recognise desugared method calls on Duration/Instant values.
    fn try_emit_time_desugared_method(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let NodeKind::FieldAccess { object, field } = &callee.kind else {
            return Ok(false);
        };
        if let NodeKind::Identifier { name } = &object.kind {
            if matches!(name.name.as_str(), "Duration" | "Instant") {
                return Ok(false);
            }
        }
        if !is_time_method_name(&field.name) {
            return Ok(false);
        }
        let remaining: Vec<bock_air::AirArg> = args.iter().skip(1).cloned().collect();
        self.try_emit_time_method(object, &field.name, &remaining)
    }

    /// Recognise `Channel.new()`, `spawn(...)`, and method calls on a
    /// channel value. Emits the Rust runtime helper equivalents using
    /// `tokio::sync::mpsc` under the hood.
    fn try_emit_concurrency_call(
        &mut self,
        callee: &AIRNode,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        if let NodeKind::Identifier { name } = &callee.kind {
            if name.name == "spawn" {
                // spawn(x) — x is expected to be an async fn invocation
                // (a Future) in Bock. In Rust we wrap it in `async move`
                // so tokio::spawn can take ownership.
                self.buf.push_str("__bock_spawn(async move { ");
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(&arg.value)?;
                    self.buf.push_str(".await");
                }
                self.buf.push_str(" })");
                return Ok(true);
            }
        }
        let NodeKind::FieldAccess { object, field } = &callee.kind else {
            return Ok(false);
        };
        if let NodeKind::Identifier { name: type_name } = &object.kind {
            if type_name.name == "Channel" && field.name == "new" {
                self.buf.push_str("__bock_channel_new()");
                return Ok(true);
            }
        }
        if matches!(field.name.as_str(), "send" | "recv" | "close") {
            self.emit_expr(object)?;
            let _ = write!(self.buf, ".{}", field.name);
            self.buf.push('(');
            for (i, arg) in args.iter().skip(1).enumerate() {
                if i > 0 {
                    self.buf.push_str(", ");
                }
                self.emit_expr(&arg.value)?;
            }
            self.buf.push(')');
            return Ok(true);
        }
        Ok(false)
    }

    /// Recognise instance methods on Duration/Instant values.
    fn try_emit_time_method(
        &mut self,
        receiver: &AIRNode,
        method: &str,
        args: &[bock_air::AirArg],
    ) -> Result<bool, CodegenError> {
        let recv_str = self.expr_to_string(receiver)?;
        let arg_strs: Vec<String> = args
            .iter()
            .map(|a| self.expr_to_string(&a.value))
            .collect::<Result<_, _>>()?;
        let code = match method {
            "as_nanos" => format!("({recv_str})"),
            "as_millis" => format!("(({recv_str}) / 1_000_000)"),
            "as_seconds" => format!("(({recv_str}) / 1_000_000_000)"),
            "is_zero" => format!("(({recv_str}) == 0)"),
            "is_negative" => format!("(({recv_str}) < 0)"),
            "abs" => format!("(({recv_str}) as i64).abs()"),
            "elapsed" => {
                format!("(({recv_str}).elapsed().as_nanos() as i64)")
            }
            "duration_since" => {
                let other = arg_strs.first().cloned().unwrap_or_default();
                format!("((({recv_str}).saturating_duration_since({other})).as_nanos() as i64)")
            }
            _ => return Ok(false),
        };
        self.buf.push_str(&code);
        Ok(true)
    }

    // ── Type emission ────────────────────────────────────────────────────────

    /// Emit an AIR type node to a Rust type string.
    /// Render a type that appears in a function signature's *param* or *return*
    /// position. A Bock `Fn(A) -> B` value there is lowered to `impl Fn(A) -> B`
    /// rather than the bare `fn(A) -> B` pointer: a Bock closure may capture
    /// (`(x) => f(g(x))` capturing `f`/`g`, the `>>`-compose and curried-call
    /// shapes), and a capturing closure does not coerce to a `fn` pointer
    /// (E0308). `impl Fn` accepts both fn-items and capturing closures. Only the
    /// outermost function type is widened — a nested `Fn` (e.g. a `Fn` returning
    /// a `Fn`) keeps the pointer form, which is rare and still coerces for the
    /// non-capturing case. Type *aliases* keep the `fn` pointer via
    /// [`Self::type_to_rs`]: `impl Trait` is not nameable in a `type` alias.
    fn type_to_rs_fn_pos(&mut self, node: &AIRNode) -> String {
        self.type_to_rs_fn_pos_bounded(node, false)
    }

    /// As [`Self::type_to_rs_fn_pos`], but when `static_bound` is set an `impl
    /// Fn` lowering gains `+ 'static`. Used for the params of a function that
    /// *returns* a closure (see [`Self::returning_fn_closure`]): the moved
    /// captures must be `'static` to satisfy the returned `impl Fn` (E0310).
    fn type_to_rs_fn_pos_bounded(&mut self, node: &AIRNode, static_bound: bool) -> String {
        if let NodeKind::TypeFunction { params, ret, .. } = &node.kind {
            let param_strs: Vec<String> = params.iter().map(|p| self.type_to_rs(p)).collect();
            let bound = if static_bound { " + 'static" } else { "" };
            format!(
                "impl Fn({}) -> {}{bound}",
                param_strs.join(", "),
                self.type_to_rs(ret)
            )
        } else {
            self.type_to_rs(node)
        }
    }

    fn type_to_rs(&mut self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                let rs_name = self.map_type_name(&name);
                if args.is_empty() {
                    rs_name
                } else {
                    let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_rs(a)).collect();
                    format!("{rs_name}<{}>", arg_strs.join(", "))
                }
            }
            NodeKind::TypeTuple { elems } => {
                let elem_strs: Vec<String> = elems.iter().map(|e| self.type_to_rs(e)).collect();
                format!("({})", elem_strs.join(", "))
            }
            NodeKind::TypeFunction { params, ret, .. } => {
                let param_strs: Vec<String> = params.iter().map(|p| self.type_to_rs(p)).collect();
                format!("fn({}) -> {}", param_strs.join(", "), self.type_to_rs(ret))
            }
            NodeKind::TypeOptional { inner } => {
                format!("Option<{}>", self.type_to_rs(inner))
            }
            NodeKind::TypeSelf => "Self".into(),
            _ => "_".into(),
        }
    }

    /// Map Bock type names to Rust equivalents.
    fn map_type_name(&mut self, name: &str) -> String {
        match name {
            "Int" => "i64".into(),
            "Float" => "f64".into(),
            "Bool" => "bool".into(),
            "Char" => "char".into(),
            "String" => "String".into(),
            "Void" | "Unit" => "()".into(),
            "List" => "Vec".into(),
            "Map" => "std::collections::HashMap".into(),
            "Set" => "std::collections::HashSet".into(),
            "Any" => "Box<dyn std::any::Any>".into(),
            "Never" => "!".into(),
            "Optional" => "Option".into(),
            "Rc" => {
                self.needs_rc_import = true;
                "Rc".into()
            }
            "Arc" => {
                self.needs_arc_import = true;
                "Arc".into()
            }
            other => other.into(),
        }
    }

    /// Emit an AST TypeExpr to a Rust type string (for record fields, etc.).
    fn ast_type_to_rs(&mut self, ty: &TypeExpr) -> String {
        match ty {
            TypeExpr::Named { path, args, .. } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                let rs_name = self.map_type_name(&name);
                if args.is_empty() {
                    rs_name
                } else {
                    let arg_strs: Vec<String> =
                        args.iter().map(|a| self.ast_type_to_rs(a)).collect();
                    format!("{rs_name}<{}>", arg_strs.join(", "))
                }
            }
            TypeExpr::Tuple { elems, .. } => {
                let elem_strs: Vec<String> = elems.iter().map(|e| self.ast_type_to_rs(e)).collect();
                format!("({})", elem_strs.join(", "))
            }
            TypeExpr::Function { params, ret, .. } => {
                let param_strs: Vec<String> =
                    params.iter().map(|p| self.ast_type_to_rs(p)).collect();
                format!(
                    "fn({}) -> {}",
                    param_strs.join(", "),
                    self.ast_type_to_rs(ret)
                )
            }
            TypeExpr::Optional { inner, .. } => {
                format!("Option<{}>", self.ast_type_to_rs(inner))
            }
            TypeExpr::SelfType { .. } => "Self".into(),
        }
    }

    /// Emit generic parameter list: `<T, U: Foo>`.
    /// Render a *use-site* generic argument list (`<T>`, `<T, U>`) — bare param
    /// names, no bounds — for a type reference like `Box<T>`. Empty for none.
    fn generic_param_args_rs(&self, params: &[bock_ast::GenericParam]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let names: Vec<&str> = params.iter().map(|p| p.name.name.as_str()).collect();
        format!("<{}>", names.join(", "))
    }

    /// Render an impl's generic-param declaration, optionally adding a `Clone`
    /// bound to every param. Used for a generic clone-target impl whose method
    /// returns `self.field` by value (the field read clones, so `T: Clone`).
    fn generic_params_to_rs_with_clone(
        &self,
        params: &[bock_ast::GenericParam],
        add_clone: bool,
    ) -> String {
        if params.is_empty() {
            return String::new();
        }
        let items: Vec<String> = params
            .iter()
            .map(|p| {
                let mut bounds: Vec<String> = p
                    .bounds
                    .iter()
                    .map(|b| self.rs_bound_to_string(b))
                    .collect();
                if add_clone && !bounds.iter().any(|b| b == "Clone") {
                    bounds.push("Clone".to_string());
                }
                if bounds.is_empty() {
                    p.name.name.clone()
                } else {
                    format!("{}: {}", p.name.name, bounds.join(" + "))
                }
            })
            .collect();
        format!("<{}>", items.join(", "))
    }

    /// The bare name of a named type expression (`Box` for `Box[T]`), dropping
    /// any generic arguments. Used to look a target up in the generic-decl
    /// registry, which is keyed by the undecorated declaration name.
    fn type_expr_base_name(&self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, .. } => path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::"),
            NodeKind::Identifier { name } => name.name.clone(),
            _ => "Unknown".into(),
        }
    }

    /// Render one generic-param bound to its Rust trait spelling, mapping a
    /// compiler-provided sealed-core trait (`Equatable`/`Comparable`/`Displayable`/
    /// `Hashable`) that has no user `impl` to its std-trait equivalent
    /// (`PartialEq`/`Ord`/`std::fmt::Display`/`Hash`) — GAP-C. A `T: Equatable`
    /// bound references a trait that does not exist in Rust, so a primitive (or
    /// any) instantiation fails `E0405`; the std equivalent lets the native
    /// `==`/`.cmp(..)`/`.to_string()` lowering type-check. A user-declared trait of
    /// the same name (a real `impl` exists) keeps its name.
    fn rs_bound_to_string(&self, b: &bock_ast::TypePath) -> String {
        let name = b
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        if crate::generator::is_unimplemented_sealed_core_trait(&name, &self.trait_decls) {
            match name.as_str() {
                "Equatable" => "PartialEq".to_string(),
                "Comparable" => "Ord".to_string(),
                "Displayable" => "std::fmt::Display".to_string(),
                "Hashable" => "std::hash::Hash".to_string(),
                _ => name,
            }
        } else {
            name
        }
    }

    fn generic_params_to_rs(&self, params: &[bock_ast::GenericParam]) -> String {
        if params.is_empty() {
            return String::new();
        }
        let items: Vec<String> = params
            .iter()
            .map(|p| {
                if p.bounds.is_empty() {
                    p.name.name.clone()
                } else {
                    let bounds: Vec<String> = p
                        .bounds
                        .iter()
                        .map(|b| self.rs_bound_to_string(b))
                        .collect();
                    format!("{}: {}", p.name.name, bounds.join(" + "))
                }
            })
            .collect();
        format!("<{}>", items.join(", "))
    }

    /// Emit where clause: `where T: Foo, U: Bar`.
    fn where_clause_to_rs(&self, clauses: &[bock_ast::TypeConstraint]) -> String {
        if clauses.is_empty() {
            return String::new();
        }
        let items: Vec<String> = clauses
            .iter()
            .map(|c| {
                let bounds: Vec<String> = c
                    .bounds
                    .iter()
                    .map(|b| self.rs_bound_to_string(b))
                    .collect();
                format!("{}: {}", c.param.name, bounds.join(" + "))
            })
            .collect();
        format!("\nwhere\n    {}", items.join(",\n    "))
    }

    // ── Top-level dispatch ──────────────────────────────────────────────────

    fn emit_node(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::Module { items, imports, .. } => {
                if self.per_module {
                    // Per-module native-module path (the real build): each module
                    // is emitted to its own `.rs` file. Record whether it
                    // references the concurrency runtime (emitted once into
                    // `bock_runtime`) and, if so, import it from there rather than
                    // inlining the prelude (a duplicate `struct __BockChannel`
                    // across files is a Rust redefinition error). Then emit real
                    // `use crate::<m>::<x>;` for cross-module references.
                    if rs_module_uses_concurrency(items) {
                        self.concurrency_runtime_emitted = true;
                        self.writeln("use crate::bock_runtime::*;");
                    }
                    self.emit_cross_module_uses(imports);
                } else {
                    // Single-module self-contained emit (`generate_module`, used
                    // by unit tests): the module's items are emitted into one file
                    // and `ImportDecl`s are dropped. The concurrency runtime is
                    // inlined at most once (a duplicate `struct __BockChannel`
                    // would not compile).
                    if !self.concurrency_runtime_emitted && rs_module_uses_concurrency(items) {
                        self.buf.push_str(CONCURRENCY_RUNTIME_RS);
                        self.buf.push('\n');
                        self.concurrency_runtime_emitted = true;
                    }
                }
                // `@test` functions are NOT emitted into the runtime module
                // tree: they are transpiled separately into the target's test
                // framework (project mode, §20.6.2 — see `generate_tests`). Their
                // bodies use the `expect(...)` assertion DSL, which has no runtime
                // definition in the emitted source, so emitting them here would
                // produce code that does not compile.
                let mut first = true;
                for item in items.iter() {
                    if crate::generator::fn_is_test(item) {
                        continue;
                    }
                    if !first {
                        self.buf.push('\n');
                    }
                    first = false;
                    self.emit_node(item)?;
                }
                Ok(())
            }
            NodeKind::ImportDecl { .. } => {
                // Resolved by the real `use crate::<m>::<x>;` statements emitted
                // up front by `emit_cross_module_uses` from the `Module` arm
                // (per-module path), or dropped entirely in the single-module
                // self-contained path. Either way, the per-item visit here is a
                // no-op.
                Ok(())
            }
            NodeKind::FnDecl {
                visibility,
                is_async,
                name,
                generic_params,
                params,
                return_type,
                effect_clause,
                where_clause,
                body,
                ..
            } => self.emit_fn_decl(
                *visibility,
                *is_async,
                &name.name,
                generic_params,
                params,
                return_type.as_deref(),
                effect_clause,
                where_clause,
                body,
            ),
            NodeKind::RecordDecl {
                visibility,
                name,
                generic_params,
                fields,
                ..
            } => {
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                // Derive `Clone` on every generated record. Bock value types are
                // freely copyable, so a generated struct must be `Clone` to be
                // usable as the type argument of a generic fn carrying a
                // `T: Clone` bound — e.g. a user `record Item` passed to
                // `first_or[T](xs: List[T], dflt: T)`, whose `List.get(i).cloned()`
                // synthesizes `where T: Clone` (GAP-B). `#[derive(Clone)]` adds
                // the standard per-field bound (a generic `Box<T>` derives
                // `Clone where T: Clone`), so this never over-constrains a
                // concrete instantiation whose fields are all `Clone`. It is the
                // only derive the backend emits, so there is no conflicting-derive
                // risk. `clone_target_records`/`clone_bound_records` continue to
                // govern the *impl bound* + `self.field.clone()` rewrite, which is
                // independent of the struct derive.
                self.writeln("#[derive(Clone)]");
                self.writeln(&format!("{vis}struct {}{generics} {{", name.name));
                self.indent += 1;
                for f in fields {
                    let ty = self.ast_type_to_rs(&f.ty);
                    self.writeln(&format!("pub {}: {ty},", to_snake_case(&f.name.name)));
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::EnumDecl {
                visibility,
                name,
                generic_params,
                variants,
                ..
            } => {
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                // Derive `Clone` on every generated enum, for the same reason as
                // records (GAP-B): a user enum used as the type argument of a
                // generic fn with a synthesized `T: Clone` bound must be `Clone`.
                self.writeln("#[derive(Clone)]");
                self.writeln(&format!("{vis}enum {}{generics} {{", name.name));
                self.indent += 1;
                for variant in variants {
                    self.emit_enum_variant(variant)?;
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::ClassDecl {
                visibility,
                name,
                generic_params,
                fields,
                methods,
                ..
            } => {
                // Rust has no classes; emit as struct + impl block.
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                // Derive `Clone` for the same reason as records/enums (GAP-B): a
                // class value used as a `T: Clone`-bounded generic argument must
                // be `Clone`.
                self.writeln("#[derive(Clone)]");
                self.writeln(&format!("{vis}struct {}{generics} {{", name.name));
                self.indent += 1;
                for f in fields {
                    let ty = self.ast_type_to_rs(&f.ty);
                    self.writeln(&format!("pub {}: {ty},", to_snake_case(&f.name.name)));
                }
                self.indent -= 1;
                self.writeln("}");
                self.buf.push('\n');
                // impl block for methods
                if !methods.is_empty() {
                    self.writeln(&format!("impl{generics} {}{generics} {{", name.name));
                    self.indent += 1;
                    for (i, method) in methods.iter().enumerate() {
                        if i > 0 {
                            self.buf.push('\n');
                        }
                        self.emit_method(method)?;
                    }
                    self.indent -= 1;
                    self.writeln("}");
                }
                Ok(())
            }
            NodeKind::TraitDecl {
                visibility,
                name,
                generic_params,
                methods,
                ..
            } => {
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                self.writeln(&format!("{vis}trait {}{generics} {{", name.name));
                self.indent += 1;
                for (i, method) in methods.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_trait_method(method)?;
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::ImplBlock {
                generic_params,
                trait_path,
                trait_args,
                target,
                where_clause,
                methods,
                ..
            } => {
                let target_base = self.type_expr_base_name(target);
                let target_rendered = self.type_expr_to_string(target);
                // Resolve the params the impl introduces. When the impl declares
                // its own (`impl[T] Box[T]`), use them and trust the target the
                // user wrote. When it declares none but the target is a generic
                // record/enum (`impl Box { ... }`, `T` on `record Box[T]`), Rust
                // requires the impl to both introduce and apply the params:
                // synthesize `impl<T> Box<T>`.
                let synth_params: Vec<bock_ast::GenericParam> = if generic_params.is_empty() {
                    self.generic_decls
                        .get(&target_base)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    generic_params.to_vec()
                };
                // A *generic* impl needs a `T: Clone` bound when the generated
                // body clones a generic value — either by moving a `self.field`
                // out by value (`return self.v` / `return Some(self.v)`, lowered
                // to `self.v.clone()`) or by calling a built-in collection method
                // the codegen lowers with `.cloned()` / `.clone()` (`List.get` /
                // `concat`, `Map.get`, a `Set` op). The pre-scan
                // `clone_target_records` already flags the bare field-return
                // getters; here we additionally cover trait impls and the
                // collection-clone case so a generic `impl P[T] for R[T]` whose
                // `f` does `return Some(self.v)`, or a generic iterator whose
                // `next` does `self.xs.get(...)`, carries the bound. Only generic
                // impls qualify (`!synth_params.is_empty()`) — a concrete record
                // moving a non-`Copy` field is the orthogonal `&self` move-out
                // defect, left untouched.
                let is_generic_impl = !synth_params.is_empty();
                let any_method_moves_self = methods
                    .iter()
                    .any(|m| matches!(&m.kind, NodeKind::FnDecl { body, .. } if Self::body_moves_self_field(body)));
                let any_method_clones_collection = methods.iter().any(|m| {
                    matches!(&m.kind, NodeKind::FnDecl { body, .. } if Self::body_clones_collection_element(body))
                });
                let add_clone_bound = is_generic_impl
                    && (self.clone_target_records.contains(&target_base)
                        || any_method_moves_self
                        || any_method_clones_collection);
                let generics = self.generic_params_to_rs_with_clone(&synth_params, add_clone_bound);
                // The applied target type. Prefer the form the user wrote if it
                // already carries args (`impl Box[T]`); otherwise synthesize
                // `Box<T>` from the recovered params.
                let target_name = if !generic_params.is_empty() || synth_params.is_empty() {
                    target_rendered
                } else {
                    format!("{target_base}{}", self.generic_param_args_rs(&synth_params))
                };
                let where_cl = self.where_clause_to_rs(where_clause);
                if let Some(tp) = trait_path {
                    let mut trait_name = tp
                        .segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("::");
                    // Trait type arguments: `impl From<Int> for Float`.
                    if !trait_args.is_empty() {
                        let args: Vec<String> =
                            trait_args.iter().map(|a| self.type_to_rs(a)).collect();
                        trait_name.push_str(&format!("<{}>", args.join(", ")));
                    }
                    self.writeln(&format!(
                        "impl{generics} {trait_name} for {target_name}{where_cl} {{"
                    ));
                } else {
                    self.writeln(&format!("impl{generics} {target_name}{where_cl} {{"));
                }
                let suppress_vis = trait_path.is_some();
                let prev_clone_self = self.in_clone_self_method;
                self.indent += 1;
                for (i, method) in methods.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    // `in_clone_self_method` controls whether a `self.field` read
                    // emits `.clone()`. Set it *per method* and only for methods
                    // that genuinely move a `self.field` out by value — never for
                    // a method that merely reads/assigns a field (cloning the LHS
                    // of `self.cursor = ...` would emit invalid Rust).
                    //
                    // A `&self` Rust method cannot move a non-`Copy` field out,
                    // so the `self.field` read is lowered to `self.field.clone()`
                    // whether the receiver is generic or concrete — e.g.
                    // `impl Iterable[Int] for Bag { fn iter(self) {
                    // list_iter(self.items) } }` moves the concrete `Vec<i64>`
                    // field out of `&self` (`E0507`). For a *generic* receiver
                    // the matching `T: Clone` bound must be in scope; the
                    // impl-level `add_clone_bound` predicate already guarantees
                    // `is_generic_impl && method_moves_self ⟹ add_clone_bound`,
                    // so dropping the conjunct here only newly enables the clone
                    // for concrete receivers (whose field type is itself
                    // clonable, no bound required).
                    let method_moves_self = matches!(
                        &method.kind,
                        NodeKind::FnDecl { body, .. } if Self::body_moves_self_field(body)
                    );
                    self.in_clone_self_method = method_moves_self;
                    self.emit_method_inner(method, suppress_vis)?;
                }
                self.indent -= 1;
                self.in_clone_self_method = prev_clone_self;
                self.writeln("}");
                Ok(())
            }
            NodeKind::EffectDecl {
                visibility,
                name,
                components,
                generic_params,
                operations,
                ..
            } => {
                if !components.is_empty() {
                    let comp_names: Vec<String> = components
                        .iter()
                        .map(|tp| {
                            tp.segments
                                .last()
                                .map_or("effect".to_string(), |s| s.name.clone())
                        })
                        .collect();
                    self.writeln(&format!(
                        "// composite effect {} = {}",
                        name.name,
                        comp_names.join(" + ")
                    ));
                    self.composite_effects.insert(name.name.clone(), comp_names);
                    return Ok(());
                }
                // Record effect operations for Call → handler.op rewriting.
                for op in operations {
                    if let NodeKind::FnDecl { name: op_name, .. } = &op.kind {
                        self.effect_ops
                            .insert(op_name.name.clone(), name.name.clone());
                    }
                }
                // Effects → Rust traits with `&dyn` usage.
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                self.writeln(&format!("{vis}trait {}{generics} {{", name.name));
                self.indent += 1;
                for (i, op) in operations.iter().enumerate() {
                    if i > 0 {
                        self.buf.push('\n');
                    }
                    self.emit_trait_method(op)?;
                }
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::TypeAlias {
                visibility,
                name,
                generic_params,
                ty,
                where_clause,
                ..
            } => {
                let vis = vis_str(*visibility);
                let generics = self.generic_params_to_rs(generic_params);
                let ty_str = self.type_to_rs(ty);
                let where_cl = self.where_clause_to_rs(where_clause);
                self.writeln(&format!(
                    "{vis}type {}{generics}{where_cl} = {ty_str};",
                    name.name
                ));
                Ok(())
            }
            NodeKind::ConstDecl {
                visibility,
                name,
                value,
                ty,
                ..
            } => {
                let vis = vis_str(*visibility);
                let ty_str = self.type_to_rs(ty);
                let ind = self.indent_str();
                let _ = write!(
                    self.buf,
                    "{ind}{vis}const {}: {ty_str} = ",
                    to_upper_snake_case(&name.name)
                );
                self.emit_expr(value)?;
                self.buf.push_str(";\n");
                Ok(())
            }
            NodeKind::ModuleHandle { effect, handler } => {
                // Module-level `handle` becomes a `const` whose type is the
                // concrete handler struct. Referring to `&CONST` in call
                // positions produces a valid `&impl Trait` borrow without
                // the `Sync`/`'static` requirements that `static &dyn Trait`
                // would impose. The handler is registered as the default
                // for this effect, so subsequent effectful calls pass it
                // implicitly unless a local handling block overrides it.
                let effect_name = effect.segments.last().map_or("effect", |s| s.name.as_str());
                let const_name = format!("__{}_HANDLER", to_snake_case(effect_name).to_uppercase());
                let handler_type = record_construct_type(handler);
                let ind = self.indent_str();
                if let Some(type_name) = handler_type {
                    let _ = write!(self.buf, "{ind}const {const_name}: {type_name} = ");
                    self.emit_expr(handler)?;
                    self.buf.push_str(";\n");
                    self.current_handler_vars
                        .insert(effect_name.to_string(), const_name);
                    // A module-level `handle` const is a concrete owned handler;
                    // forwarding it borrows (`&CONST`), so it is not a borrowed
                    // param.
                    self.borrowed_handler_effects.remove(effect_name);
                } else {
                    // Fallback for non-literal handlers: emit a comment so the
                    // output is still valid Rust but the handler must be
                    // provided at every call site.
                    let _ = write!(self.buf, "{ind}// module handle: {effect_name} with ");
                    self.emit_expr(handler)?;
                    self.buf.push('\n');
                }
                Ok(())
            }
            NodeKind::PropertyTest { name, .. } => {
                self.writeln(&format!("// property test: {name}"));
                Ok(())
            }
            // Statement / expression nodes at top level:
            NodeKind::LetBinding { .. }
            | NodeKind::If { .. }
            | NodeKind::For { .. }
            | NodeKind::While { .. }
            | NodeKind::Loop { .. }
            | NodeKind::Return { .. }
            | NodeKind::Break { .. }
            | NodeKind::Continue
            | NodeKind::Guard { .. }
            | NodeKind::Match { .. }
            | NodeKind::Block { .. }
            | NodeKind::HandlingBlock { .. }
            | NodeKind::Assign { .. } => self.emit_stmt(node),
            // Expression nodes that appear as statements:
            _ => {
                self.write_indent();
                self.emit_expr(node)?;
                self.buf.push_str(";\n");
                Ok(())
            }
        }
    }

    // ── Function declarations ───────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn emit_fn_decl(
        &mut self,
        visibility: Visibility,
        is_async: bool,
        name: &str,
        generic_params: &[bock_ast::GenericParam],
        params: &[AIRNode],
        return_type: Option<&AIRNode>,
        effect_clause: &[bock_ast::TypePath],
        where_clause: &[bock_ast::TypeConstraint],
        body: &AIRNode,
    ) -> Result<(), CodegenError> {
        let vis = vis_str(visibility);
        let async_kw = if is_async { "async " } else { "" };
        // A generic free function whose body clones a generic element via a
        // built-in collection method (`List.get`/`concat`, `Map.get`, a `Set`
        // op — each lowered with `.cloned()` / `.clone()`) needs a `T: Clone`
        // bound, just like the generic-impl case. Without it `dup[T](xs:
        // List[T])` returning `xs.concat(xs)` fails with `E0277: T: Clone is not
        // satisfied`. Only generic functions qualify, and only when such a clone
        // is actually emitted.
        //
        // It also needs the bound *transitively*: a fn that takes a clone-bound
        // record by value (`ListIterator[T]`, whose `impl` requires `T: Clone`)
        // and drives it with a method call must propagate that bound, or
        // method resolution fails (`count[T]`/`fold[T,A]` calling `it.next()` →
        // `E0599`). See `params_drive_clone_bound_record`.
        let add_clone_bound = !generic_params.is_empty()
            && (Self::body_clones_collection_element(body)
                || Self::body_reuses_match_binding(body)
                || self.params_drive_clone_bound_record(params, body));
        let generics = self.generic_params_to_rs_with_clone(generic_params, add_clone_bound);
        // A function whose declared return type is a `Fn(..) -> ..` returns an
        // `impl Fn` (a closure). Its closure params then need `+ 'static` and the
        // tail closure must `move`-capture — see `returning_fn_closure`.
        let returns_fn_closure =
            return_type.is_some_and(|t| matches!(&t.kind, NodeKind::TypeFunction { .. }));
        let param_strs = if returns_fn_closure {
            self.collect_param_strs_static_fn(params)
        } else {
            self.collect_param_strs(params)
        };
        let effects = self.effects_params(effect_clause);
        let mut all_params = param_strs;
        all_params.extend(effects);
        let ret = return_type
            .map(|t| format!(" -> {}", self.type_to_rs_fn_pos(t)))
            .unwrap_or_default();
        let where_cl = self.where_clause_to_rs(where_clause);
        if !effect_clause.is_empty() {
            let effect_names = self.expand_effect_names(effect_clause);
            self.fn_effects.insert(name.to_string(), effect_names);
        }
        let fn_name = to_snake_case(name);
        // `async fn main` needs a runtime attribute — tokio drives the future
        // to completion on a multi-threaded executor, matching the Bock
        // interpreter's async runtime model.
        if is_async && fn_name == "main" {
            self.writeln("#[tokio::main]");
        }
        self.writeln(&format!(
            "{vis}{async_kw}fn {fn_name}{generics}({}){ret}{where_cl} {{",
            all_params.join(", "),
        ));
        self.indent += 1;
        let old_handler_vars = self.current_handler_vars.clone();
        let old_borrowed_handlers = self.borrowed_handler_effects.clone();
        let expanded = self.expand_effect_names(effect_clause);
        for ename in &expanded {
            self.current_handler_vars
                .insert(ename.clone(), to_snake_case(ename));
            // This fn's effect param is `&impl Effect` — already a reference, so
            // a nested effectful call forwards it as-is (not re-borrowed).
            self.borrowed_handler_effects.insert(ename.clone());
        }
        // A by-value, non-`Copy` parameter reused after a move must clone on each
        // by-value pass (`E0382`). See `seed_reused_params`.
        let seeded = self.seed_reused_params(params, body);
        let prev_returning = self.return_closure_tail;
        // The flag is consulted only at the function's tail expression (the
        // returned value), so an intermediate `.map`/`.filter` closure in the
        // body is unaffected — only the returned closure gets `move`. See
        // `returning_fn_closure` / `return_closure_tail`.
        self.return_closure_tail = returns_fn_closure;
        self.emit_block_body(body)?;
        self.return_closure_tail = prev_returning;
        for name in seeded {
            self.reused_let_bindings.remove(&name);
        }
        self.current_handler_vars = old_handler_vars;
        self.borrowed_handler_effects = old_borrowed_handlers;
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    /// Emit a method inside an impl block (with `&self` / `&mut self`).
    /// If `suppress_vis` is true, visibility qualifiers are omitted (e.g. trait impl methods).
    fn emit_method_inner(
        &mut self,
        method: &AIRNode,
        suppress_vis: bool,
    ) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            visibility,
            is_async,
            name,
            generic_params,
            params,
            return_type,
            effect_clause,
            where_clause,
            body,
            ..
        } = &method.kind
        {
            let vis = if suppress_vis {
                ""
            } else {
                vis_str(*visibility)
            };
            let async_kw = if *is_async { "async " } else { "" };
            let generics = self.generic_params_to_rs(generic_params);
            // The AIR keeps `self` as a leading `Param`; consume it to form the
            // native Rust receiver and emit the remaining params positionally.
            // Without this the method gets both `&self` and a `self: _` param.
            let (receiver, rest) = match params.first().map(crate::generator::param_binds_self) {
                Some(Some(is_mut)) => {
                    let recv = if is_mut { "&mut self" } else { "&self" };
                    (recv.to_string(), &params[1..])
                }
                _ => ("&self".to_string(), &params[..]),
            };
            // A `Self`-operand trait method's impl borrows its operand to match
            // the trait signature (`fn compare(&self, other: &Key)`); the call
            // site borrows the argument to match. See `self_operand_methods`.
            let borrow_operands = self.self_operand_methods.contains(&name.name);
            let param_strs = self.collect_param_strs_inner(rest, borrow_operands, false);
            let effects = self.effects_params(effect_clause);
            let mut all_params = vec![receiver];
            all_params.extend(param_strs);
            all_params.extend(effects);
            let ret = return_type
                .as_deref()
                .map(|t| format!(" -> {}", self.type_to_rs(t)))
                .unwrap_or_default();
            let where_cl = self.where_clause_to_rs(where_clause);
            let fn_name = to_snake_case(&name.name);
            self.writeln(&format!(
                "{vis}{async_kw}fn {fn_name}{generics}({}){ret}{where_cl} {{",
                all_params.join(", "),
            ));
            self.indent += 1;
            let old_handler_vars = self.current_handler_vars.clone();
            let old_borrowed_handlers = self.borrowed_handler_effects.clone();
            let expanded = self.expand_effect_names(effect_clause);
            for ename in &expanded {
                self.current_handler_vars
                    .insert(ename.clone(), to_snake_case(ename));
                // `&impl Effect` method param — forward as-is, never re-borrowed.
                self.borrowed_handler_effects.insert(ename.clone());
            }
            // Seed move-reuse clones for by-value, non-`Copy` method params (the
            // `self` receiver is borrowed and skipped). See `seed_reused_params`.
            let seeded = self.seed_reused_params(rest, body);
            self.emit_block_body(body)?;
            for name in seeded {
                self.reused_let_bindings.remove(&name);
            }
            self.current_handler_vars = old_handler_vars;
            self.borrowed_handler_effects = old_borrowed_handlers;
            self.indent -= 1;
            self.writeln("}");
        }
        Ok(())
    }

    /// Emit a method with visibility preserved.
    fn emit_method(&mut self, method: &AIRNode) -> Result<(), CodegenError> {
        self.emit_method_inner(method, false)
    }

    /// Emit a trait method signature (may or may not have a body).
    fn emit_trait_method(&mut self, method: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::FnDecl {
            is_async,
            name,
            generic_params,
            params,
            return_type,
            effect_clause,
            where_clause,
            body,
            ..
        } = &method.kind
        {
            let async_kw = if *is_async { "async " } else { "" };
            let generics = self.generic_params_to_rs(generic_params);
            let (receiver, rest) = match params.first().map(crate::generator::param_binds_self) {
                Some(Some(is_mut)) => {
                    let recv = if is_mut { "&mut self" } else { "&self" };
                    (recv.to_string(), &params[1..])
                }
                _ => ("&self".to_string(), &params[..]),
            };
            // A `Self`-operand trait method (`compare`/`eq`/…) takes its operand
            // by shared reference, so the bound value can be reused after the
            // call (Bock value semantics) — see `self_operand_methods`.
            let borrow_operands = self.self_operand_methods.contains(&name.name);
            let param_strs = self.collect_param_strs_inner(rest, borrow_operands, false);
            let effects = self.effects_params(effect_clause);
            let mut all_params = vec![receiver];
            all_params.extend(param_strs);
            all_params.extend(effects);
            let ret = return_type
                .as_deref()
                .map(|t| format!(" -> {}", self.type_to_rs(t)))
                .unwrap_or_default();
            let mut where_cl = self.where_clause_to_rs(where_clause);
            let fn_name = to_snake_case(&name.name);

            // A default method (one carrying a body) that still takes a `Self`
            // operand *by value* needs `where Self: Sized` (inside the trait
            // `Self` is `?Sized`). A borrowed operand (`other: &Self`) is always
            // sized, so the bound is unnecessary there.
            let has_body = crate::generator::is_default_method(method);
            if has_body && !borrow_operands && rest.iter().any(Self::param_type_is_self) {
                if where_cl.is_empty() {
                    where_cl = " where Self: Sized".to_string();
                } else {
                    where_cl = format!("{where_cl},\n    Self: Sized");
                }
            }

            if has_body {
                self.writeln(&format!(
                    "{async_kw}fn {fn_name}{generics}({}){ret}{where_cl} {{",
                    all_params.join(", "),
                ));
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
            } else {
                self.writeln(&format!(
                    "{async_kw}fn {fn_name}{generics}({}){ret}{where_cl};",
                    all_params.join(", "),
                ));
            }
        }
        Ok(())
    }

    /// True if `param` is a `Param` node whose declared type is exactly `Self`.
    /// Used to decide whether a default trait method needs `where Self: Sized`
    /// (a by-value `Self` operand is `?Sized` inside the trait).
    fn param_type_is_self(param: &AIRNode) -> bool {
        matches!(
            &param.kind,
            NodeKind::Param { ty: Some(t), .. } if matches!(t.kind, NodeKind::TypeSelf)
        )
    }

    fn collect_param_strs(&mut self, params: &[AIRNode]) -> Vec<String> {
        self.collect_param_strs_inner(params, false, false)
    }

    /// As [`Self::collect_param_strs`], but every `Fn`-typed param gains a
    /// `+ 'static` bound on its `impl Fn` lowering. Used for the params of a
    /// function that returns a closure — see [`Self::returning_fn_closure`].
    fn collect_param_strs_static_fn(&mut self, params: &[AIRNode]) -> Vec<String> {
        self.collect_param_strs_inner(params, false, true)
    }

    /// As [`Self::collect_param_strs`], but when `borrow` is set each param's
    /// declared type is emitted as a shared reference (`other: &Target`). Used
    /// for the operands of a `Self`-operand trait method (`compare`/`eq`/…),
    /// which Rust must take by reference to match Bock's value semantics. When
    /// `static_fn` is set, a `Fn`-typed param's `impl Fn` lowering gains
    /// `+ 'static` (a closure-returning function — see `collect_param_strs_static_fn`).
    fn collect_param_strs_inner(
        &mut self,
        params: &[AIRNode],
        borrow: bool,
        static_fn: bool,
    ) -> Vec<String> {
        let mut result = Vec::new();
        for p in params {
            if let NodeKind::Param {
                pattern,
                ty,
                default,
            } = &p.kind
            {
                let name = to_snake_case(&self.pattern_to_binding_name(pattern));
                // A `mut`-bound param (`fn f(mut items: …)`) may reassign its
                // binding in the body (`items = …`). Rust requires the binding
                // be declared `mut` or the reassignment is E0384. The borrow
                // form (`&Target`) takes the operand by shared reference and is
                // never reassigned, so it never gets `mut`.
                let mut_kw =
                    if !borrow && matches!(&pattern.kind, NodeKind::BindPat { is_mut: true, .. }) {
                        "mut "
                    } else {
                        ""
                    };
                let amp = if borrow { "&" } else { "" };
                let type_ann = ty
                    .as_ref()
                    .map(|t| format!(": {amp}{}", self.type_to_rs_fn_pos_bounded(t, static_fn)))
                    .unwrap_or_else(|| ": _".into());
                if let Some(def) = default {
                    // Rust doesn't have default params; emit a comment.
                    let mut ctx = RsEmitCtx::new();
                    ctx.indent = self.indent;
                    if ctx.emit_expr(def).is_ok() {
                        let def_str = ctx.buf;
                        result.push(format!("{mut_kw}{name}{type_ann} /* = {def_str} */"));
                        continue;
                    }
                }
                result.push(format!("{mut_kw}{name}{type_ann}"));
            }
        }
        result
    }

    /// Expand effect names, replacing composite effects with their components.
    fn expand_effect_names(&self, effects: &[bock_ast::TypePath]) -> Vec<String> {
        let mut result = Vec::new();
        for tp in effects {
            let name = tp
                .segments
                .last()
                .map_or("effect".to_string(), |s| s.name.clone());
            if let Some(components) = self.composite_effects.get(&name) {
                result.extend(components.iter().cloned());
            } else {
                result.push(name);
            }
        }
        result
    }

    /// Effects → `&impl EffectTrait` parameters (argument-position impl trait).
    /// This gives each effectful function a fresh generic parameter per effect,
    /// so handlers can be any concrete type implementing the effect trait while
    /// keeping the ownership story simple: the caller retains ownership and the
    /// function borrows for its body.
    fn effects_params(&self, effects: &[bock_ast::TypePath]) -> Vec<String> {
        let expanded = self.expand_effect_names(effects);
        expanded
            .iter()
            .map(|name| {
                let param_name = to_snake_case(name);
                format!("{param_name}: &impl {name}")
            })
            .collect()
    }

    /// Build the handler arguments for calling an effectful function. Each effect
    /// of the callee is forwarded from the current scope's handler variable: a
    /// concrete owned handler (module-level `handle` const, a `handling`-block
    /// local) is borrowed (`&handler`); an *already-borrowed* `&impl Effect`
    /// parameter is forwarded as-is (`handler`), since re-borrowing it would be
    /// `&&impl Effect` and fail the trait bound (`E0277`). See
    /// [`Self::borrowed_handler_effects`].
    fn build_effects_call_args_rs(&self, fn_name: &str) -> Option<String> {
        let effects = self.fn_effects.get(fn_name)?;
        let entries: Vec<String> = effects
            .iter()
            .filter_map(|e| {
                let handler_var = self.current_handler_vars.get(e)?;
                if self.borrowed_handler_effects.contains(e) {
                    Some(handler_var.clone())
                } else {
                    Some(format!("&{handler_var}"))
                }
            })
            .collect();
        if entries.is_empty() {
            return None;
        }
        Some(entries.join(", "))
    }

    // ── Enum variant ────────────────────────────────────────────────────────

    fn emit_enum_variant(&mut self, variant: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::EnumVariant { name, payload } = &variant.kind {
            let vname = &name.name;
            match payload {
                EnumVariantPayload::Unit => {
                    self.writeln(&format!("{vname},"));
                }
                EnumVariantPayload::Struct(fields) => {
                    self.writeln(&format!("{vname} {{"));
                    self.indent += 1;
                    for f in fields {
                        let ty = self.ast_type_to_rs(&f.ty);
                        self.writeln(&format!("{}: {ty},", to_snake_case(&f.name.name)));
                    }
                    self.indent -= 1;
                    self.writeln("},");
                }
                EnumVariantPayload::Tuple(elems) => {
                    let types: Vec<String> = elems.iter().map(|e| self.type_to_rs(e)).collect();
                    self.writeln(&format!("{vname}({}),", types.join(", ")));
                }
            }
        }
        Ok(())
    }

    // ── Statements ──────────────────────────────────────────────────────────

    fn emit_stmt(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::LetBinding {
                pattern,
                value,
                ty,
                is_mut,
            } => {
                // Declare-only temp from the shared value-CF hoist: Rust allows a
                // deferred-init `let mut x;` when every path assigns before use,
                // which the relocated control flow guarantees. The type is
                // inferred from the assignment(s).
                if node.metadata.contains_key(crate::generator::DECL_ONLY_META) {
                    let binding = self.pattern_to_rs_binding(pattern);
                    let ind = self.indent_str();
                    let type_ann = ty
                        .as_ref()
                        .map(|t| format!(": {}", self.type_to_rs(t)))
                        .unwrap_or_default();
                    let _ = writeln!(self.buf, "{ind}let mut {binding}{type_ann};");
                    return Ok(());
                }
                let binding = self.pattern_to_rs_binding(pattern);
                let type_ann = ty
                    .as_ref()
                    .map(|t| format!(": {}", self.type_to_rs(t)))
                    .unwrap_or_default();
                let mut_kw = if *is_mut { "mut " } else { "" };
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}let {mut_kw}{binding}{type_ann} = ");
                let wrap_task = matches!(&value.kind, NodeKind::Call { .. })
                    && self.task_bound_names.contains(&binding);
                if wrap_task {
                    self.buf.push_str("tokio::spawn(");
                    self.emit_expr(value)?;
                    self.buf.push(')');
                } else {
                    self.emit_expr(value)?;
                }
                self.buf.push_str(";\n");
                Ok(())
            }
            NodeKind::If {
                let_pattern,
                condition,
                then_block,
                else_block,
            } => {
                let ind = self.indent_str();
                if let Some(pat) = let_pattern {
                    let binding = self.pattern_to_rs_binding(pat);
                    let _ = write!(self.buf, "{ind}if let Some({binding}) = ");
                    self.emit_expr(condition)?;
                    self.buf.push_str(" {\n");
                } else {
                    let _ = write!(self.buf, "{ind}if ");
                    self.emit_expr(condition)?;
                    self.buf.push_str(" {\n");
                }
                self.indent += 1;
                self.emit_block_body(then_block)?;
                self.indent -= 1;
                if let Some(else_b) = else_block {
                    if matches!(else_b.kind, NodeKind::If { .. }) {
                        let ind = self.indent_str();
                        let _ = write!(self.buf, "{ind}}} else ");
                        // Remove leading indent from recursive call
                        self.emit_if_continuing(else_b)?;
                        return Ok(());
                    }
                    self.writeln("} else {");
                    self.indent += 1;
                    self.emit_block_body(else_b)?;
                    self.indent -= 1;
                }
                self.writeln("}");
                Ok(())
            }
            NodeKind::For {
                pattern,
                iterable,
                body,
            } => {
                let binding = self.pattern_to_rs_binding(pattern);
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}for {binding} in ");
                // `for x in coll` consumes `coll` via `.into_iter()`. If `coll` is
                // a binding reused after the loop (`render_document(nodes)` after
                // `for n in nodes`), clone it so the later use stays live
                // (`E0382`). The loop body keeps owned element bindings (matching
                // Bock's by-value `for`); iterating `&coll` would change them to
                // references and break the body.
                //
                // The iterable may be a *field access* of a reused binding
                // (`for row in dataset.rows` while `dataset` is used again after
                // the loop): iterating `dataset.rows` partially moves `dataset`,
                // so a later `dataset.clone()`/`dataset.field` is a use of a
                // partially-moved value (`E0382`). Cloning the field access
                // (`dataset.rows.clone()`) leaves the owner intact.
                let clone_iter = self.iterable_is_reused(iterable);
                self.emit_expr(iterable)?;
                if clone_iter {
                    self.buf.push_str(".clone()");
                }
                self.buf.push_str(" {\n");
                self.indent += 1;
                // A loop body's tail is not a function return — disarm the
                // closure-return `move` so a loop-tail closure isn't `move`d.
                let prev_tail = std::mem::replace(&mut self.return_closure_tail, false);
                // The loop variable binds an owned, by-value element each
                // iteration (Bock's by-value `for`). If the body passes it
                // by value to a call and *also* reads it afterward
                // (`is_category(e, …)` then `e.amount`), the first pass moves it
                // (`E0382`). Seed it as a reused binding so the call-arg emitter
                // clones the by-value pass. Only when read more than once and not
                // a `Copy` scalar (cloning those is needless noise).
                let prev_reused = self.reused_let_bindings.clone();
                let mut loop_bindings = Vec::new();
                Self::collect_pattern_binding_names(pattern, &mut loop_bindings);
                for b in &loop_bindings {
                    let rs_name = to_snake_case(b);
                    if Self::count_identifier_uses(body, &rs_name) > 1 {
                        self.reused_let_bindings.insert(rs_name);
                    }
                }
                self.emit_block_body(body)?;
                self.reused_let_bindings = prev_reused;
                self.return_closure_tail = prev_tail;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::While { condition, body } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}while ");
                self.emit_expr(condition)?;
                self.buf.push_str(" {\n");
                self.indent += 1;
                let prev_tail = std::mem::replace(&mut self.return_closure_tail, false);
                self.emit_block_body(body)?;
                self.return_closure_tail = prev_tail;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Loop { body } => {
                self.writeln("loop {");
                self.indent += 1;
                let prev_tail = std::mem::replace(&mut self.return_closure_tail, false);
                self.emit_block_body(body)?;
                self.return_closure_tail = prev_tail;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Return { value } => {
                if let Some(val) = value {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}return ");
                    self.emit_expr(val)?;
                    self.buf.push_str(";\n");
                } else {
                    self.writeln("return;");
                }
                Ok(())
            }
            NodeKind::Break { value } => {
                if let Some(val) = value {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}break ");
                    self.emit_expr(val)?;
                    self.buf.push_str(";\n");
                } else {
                    self.writeln("break;");
                }
                Ok(())
            }
            NodeKind::Continue => {
                self.writeln("continue;");
                Ok(())
            }
            NodeKind::Guard {
                let_pattern,
                condition,
                else_block,
            } => {
                let ind = self.indent_str();
                if let Some(pat) = let_pattern {
                    // `guard (let PAT = EXPR) else { … }` lowers to Rust's
                    // `let-else`: `let PAT = EXPR else { … };`. The pattern's
                    // bindings stay in scope for the rest of the enclosing
                    // block (the whole point of guard-let), and the else arm
                    // must diverge — which Bock's guard semantics already
                    // guarantee. Lowering it to a boolean `if !(cond)` instead
                    // drops the bindings (E0425) and negates a non-bool value
                    // (E0600); `let-else` is the faithful form.
                    let _ = write!(self.buf, "{ind}let ");
                    self.emit_pattern(pat)?;
                    self.buf.push_str(" = ");
                    self.emit_expr(condition)?;
                    self.buf.push_str(" else {\n");
                    self.indent += 1;
                    self.emit_block_body(else_block)?;
                    self.indent -= 1;
                    self.writeln("};");
                } else {
                    let _ = write!(self.buf, "{ind}if !(");
                    self.emit_expr(condition)?;
                    self.buf.push_str(") {\n");
                    self.indent += 1;
                    self.emit_block_body(else_block)?;
                    self.indent -= 1;
                    self.writeln("}");
                }
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => self.emit_match(scrutinee, arms),
            NodeKind::Block { stmts, tail } => {
                for s in stmts {
                    self.emit_node(s)?;
                }
                if let Some(t) = tail {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push('\n');
                }
                Ok(())
            }
            NodeKind::HandlingBlock { handlers, body } => {
                // handling block → scoped handler instantiation
                self.writeln("{");
                self.indent += 1;
                let old_handler_vars = self.current_handler_vars.clone();
                let old_borrowed_handlers = self.borrowed_handler_effects.clone();
                for h in handlers {
                    let effect_name = h
                        .effect
                        .segments
                        .last()
                        .map_or("effect", |s| s.name.as_str());
                    let var_name = format!("__{}", to_snake_case(effect_name));
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}let {var_name} = ");
                    self.emit_expr(&h.handler)?;
                    self.buf.push_str(";\n");
                    self.current_handler_vars
                        .insert(effect_name.to_string(), var_name);
                    // A `handling`-block local is a concrete owned handler value,
                    // so forwarding it borrows (`&__effect`) — clear any inherited
                    // borrowed-param marker for this effect.
                    self.borrowed_handler_effects.remove(effect_name);
                }
                if let NodeKind::Block { stmts, tail } = &body.kind {
                    for s in stmts {
                        self.emit_node(s)?;
                    }
                    if let Some(t) = tail {
                        self.write_indent();
                        self.emit_expr(t)?;
                        self.buf.push('\n');
                    }
                } else {
                    self.emit_stmt(body)?;
                }
                self.current_handler_vars = old_handler_vars;
                self.borrowed_handler_effects = old_borrowed_handlers;
                self.indent -= 1;
                self.writeln("}");
                Ok(())
            }
            NodeKind::Assign { op, target, value } => {
                let ind = self.indent_str();
                let _ = write!(self.buf, "{ind}");
                // The target is a place expression; suppress the clone-self
                // `self.field` → `self.field.clone()` rewrite while emitting it.
                let prev_assign_target = self.in_assign_target;
                self.in_assign_target = true;
                let target_res = self.emit_expr(target);
                self.in_assign_target = prev_assign_target;
                target_res?;
                let op_str = match op {
                    AssignOp::Assign => " = ",
                    AssignOp::AddAssign => " += ",
                    AssignOp::SubAssign => " -= ",
                    AssignOp::MulAssign => " *= ",
                    AssignOp::DivAssign => " /= ",
                    AssignOp::RemAssign => " %= ",
                };
                self.buf.push_str(op_str);
                self.emit_expr(value)?;
                self.buf.push_str(";\n");
                Ok(())
            }
            _ => {
                self.write_indent();
                self.emit_expr(node)?;
                self.buf.push_str(";\n");
                Ok(())
            }
        }
    }

    /// Helper for chained if/else if without extra indent.
    fn emit_if_continuing(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::If {
            let_pattern,
            condition,
            then_block,
            else_block,
        } = &node.kind
        {
            if let Some(pat) = let_pattern {
                let binding = self.pattern_to_rs_binding(pat);
                let _ = write!(self.buf, "if let Some({binding}) = ");
                self.emit_expr(condition)?;
                self.buf.push_str(" {\n");
            } else {
                let _ = write!(self.buf, "if ");
                self.emit_expr(condition)?;
                self.buf.push_str(" {\n");
            }
            self.indent += 1;
            self.emit_block_body(then_block)?;
            self.indent -= 1;
            if let Some(else_b) = else_block {
                if matches!(else_b.kind, NodeKind::If { .. }) {
                    let ind = self.indent_str();
                    let _ = write!(self.buf, "{ind}}} else ");
                    self.emit_if_continuing(else_b)?;
                    return Ok(());
                }
                self.writeln("} else {");
                self.indent += 1;
                self.emit_block_body(else_b)?;
                self.indent -= 1;
            }
            self.writeln("}");
        }
        Ok(())
    }

    // ── Expressions ─────────────────────────────────────────────────────────

    fn emit_expr(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        match &node.kind {
            NodeKind::Literal { lit } => {
                match lit {
                    Literal::Int(s) => {
                        self.buf.push_str(s);
                        self.buf.push_str("_i64");
                    }
                    Literal::Float(s) => {
                        self.buf.push_str(s);
                        self.buf.push_str("_f64");
                    }
                    Literal::Bool(b) => {
                        self.buf.push_str(if *b { "true" } else { "false" });
                    }
                    Literal::Char(s) => {
                        self.buf.push('\'');
                        self.buf.push_str(s);
                        self.buf.push('\'');
                    }
                    Literal::String(s) => {
                        self.buf.push('"');
                        self.buf.push_str(&escape_rs_string(s));
                        self.buf.push('"');
                        self.buf.push_str(".to_string()");
                    }
                    Literal::Unit => self.buf.push_str("()"),
                }
                Ok(())
            }
            NodeKind::Identifier { name } => {
                // The prelude `Ordering` variants map to Rust's native
                // `std::cmp::Ordering` — UNLESS the real `core.compare.Ordering`
                // enum is reachable, in which case the references must use that
                // user enum (handled by the `variant_enum_qualifier_for_name`
                // path below). This mirrors how `Some`/`None` map to
                // `std::option`.
                if crate::generator::ordering_variant(&name.name).is_some()
                    && !self.ordering_enum_reachable()
                {
                    let variant = &name.name;
                    let _ = write!(self.buf, "std::cmp::Ordering::{variant}");
                    return Ok(());
                }
                // A bare identifier naming a registered unit variant is a
                // construction (`Empty` → `Shape::Empty`); Rust requires the
                // enum qualifier.
                if let Some(enum_name) = self.variant_enum_qualifier_for_name(&name.name) {
                    let _ = write!(self.buf, "{enum_name}::{}", name.name);
                } else {
                    self.buf.push_str(&identifier_to_rs(&name.name));
                }
                Ok(())
            }
            NodeKind::BinaryOp { op, left, right } => {
                // `+` on two `List[T]` operands is concatenation. Rust does not
                // implement `Add` for `Vec<T>` (E0369), so clone the left operand
                // and extend it with the right — value-semantic concat that leaves
                // both operands usable. `T: Clone` holds for every v1 element type.
                if *op == BinOp::Add && crate::generator::is_list_concat(node, left, right) {
                    let l = self.expr_to_string(left)?;
                    let r = self.expr_to_string(right)?;
                    let _ = write!(
                        self.buf,
                        "{{ let mut __v = ({l}).clone(); __v.extend(({r}).iter().cloned()); __v }}"
                    );
                    return Ok(());
                }
                // `String + String` concat: Rust's `String + String` does not
                // compile (`Add<String>` is not implemented; only `String +
                // &str`). Emit `format!("{}{}", l, r)`, which concatenates
                // regardless of whether each side is an owned `String` or `&str`.
                // The checker's `string_concat` stamp is authoritative (it sees
                // operand *types*, so it catches `result + sep` where both are
                // `String`-typed identifiers); the syntactic `expr_is_string_rs`
                // heuristic is the fallback for unstamped nodes.
                let string_concat_stamped = matches!(
                    node.metadata
                        .get(bock_types::checker::STRING_CONCAT_META_KEY),
                    Some(bock_air::Value::Bool(true))
                );
                if *op == BinOp::Add
                    && (string_concat_stamped
                        || Self::expr_is_string_rs(left)
                        || Self::expr_is_string_rs(right))
                {
                    let l = self.expr_to_string(left)?;
                    let r = self.expr_to_string(right)?;
                    let _ = write!(self.buf, "format!(\"{{}}{{}}\", {l}, {r})");
                    return Ok(());
                }
                // `**` has no Rust operator. An `Int ** Int` lowers to
                // `i64::pow`, whose exponent is `u32` (not `i64`) — so the
                // exponent is cast `(rhs) as u32` (E0308 otherwise). A
                // `Float ** _` lowers to `f64::powf`, whose exponent is `f64`;
                // a Float-literal operand on either side selects this path so
                // `b ** 3.0` type-checks instead of calling the (nonexistent)
                // `f64::pow`. The float exponent is cast `(rhs) as f64` to
                // admit an integer-literal exponent (`2.0 ** 3`).
                if *op == BinOp::Pow {
                    if Self::pow_is_float(left, right) {
                        self.buf.push('(');
                        self.emit_expr(left)?;
                        self.buf.push_str(").powf((");
                        self.emit_expr(right)?;
                        self.buf.push_str(") as f64)");
                    } else {
                        self.buf.push('(');
                        self.emit_expr(left)?;
                        self.buf.push_str(").pow((");
                        self.emit_expr(right)?;
                        self.buf.push_str(") as u32)");
                    }
                    return Ok(());
                }
                self.buf.push('(');
                self.emit_expr(left)?;
                let op_str = match op {
                    BinOp::Add => " + ",
                    BinOp::Sub => " - ",
                    BinOp::Mul => " * ",
                    BinOp::Div => " / ",
                    BinOp::Rem => " % ",
                    BinOp::Pow => unreachable!("Pow handled above"),
                    BinOp::Eq => " == ",
                    BinOp::Ne => " != ",
                    BinOp::Lt => " < ",
                    BinOp::Le => " <= ",
                    BinOp::Gt => " > ",
                    BinOp::Ge => " >= ",
                    BinOp::And => " && ",
                    BinOp::Or => " || ",
                    BinOp::BitAnd => " & ",
                    BinOp::BitOr => " | ",
                    BinOp::BitXor => " ^ ",
                    BinOp::Compose => " /* compose */ ",
                    BinOp::Is => " /* is */ ",
                };
                self.buf.push_str(op_str);
                self.emit_expr(right)?;
                self.buf.push(')');
                Ok(())
            }
            NodeKind::UnaryOp { op, operand } => {
                let op_str = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                    UnaryOp::BitNot => "!",
                };
                self.buf.push_str(op_str);
                self.emit_expr(operand)?;
                Ok(())
            }
            NodeKind::Call { callee, args, .. } => {
                // Rewrite bare effect operation calls: log(...) → handler.log(...)
                if let NodeKind::Identifier { name } = &callee.kind {
                    if let Some(effect_name) = self.effect_ops.get(&name.name).cloned() {
                        if let Some(handler_var) =
                            self.current_handler_vars.get(&effect_name).cloned()
                        {
                            let _ =
                                write!(self.buf, "{}.{}", handler_var, to_snake_case(&name.name));
                            self.buf.push('(');
                            for (i, arg) in args.iter().enumerate() {
                                if i > 0 {
                                    self.buf.push_str(", ");
                                }
                                // A by-value pass of a reused binding into an
                                // effect op (`storage.write(key, value)` before a
                                // later `format!("…", key)`) moves it; clone (or
                                // borrow a reused closure) so the later use stays
                                // live (`E0382`/`E0599`). See `emit_call_arg`.
                                self.emit_call_arg(&arg.value, false)?;
                            }
                            self.buf.push(')');
                            return Ok(());
                        }
                    }
                }
                if let Some(code) = self.map_prelude_call(callee, args)? {
                    self.buf.push_str(&code);
                    return Ok(());
                }
                // A call whose callee names a registered tuple variant is a
                // construction (`Rect(3.0, 4.0)` → `Shape::Rect(3.0, 4.0)`).
                if let NodeKind::Identifier { name } = &callee.kind {
                    if let Some(enum_name) = self.variant_enum_qualifier_for_name(&name.name) {
                        let _ = write!(self.buf, "{enum_name}::{}(", name.name);
                        for (i, arg) in args.iter().enumerate() {
                            if i > 0 {
                                self.buf.push_str(", ");
                            }
                            self.emit_expr(&arg.value)?;
                        }
                        self.buf.push(')');
                        return Ok(());
                    }
                }
                if self.try_emit_time_assoc_call(callee, args)? {
                    return Ok(());
                }
                if self.try_emit_time_desugared_method(callee, args)? {
                    return Ok(());
                }
                if self.try_emit_concurrency_call(callee, args)? {
                    return Ok(());
                }
                // Map/Set dispatch precedes the List recogniser so the
                // overlapping method names route by `recv_kind`, not by name.
                if self.try_emit_map_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_set_method(node, callee, args)? {
                    return Ok(());
                }
                // String method dispatch runs *before* the List recogniser so the
                // overlapping `len`/`contains`/`is_empty` names route by the
                // checker's `recv_kind = "Primitive:String"`, not by name alone.
                if self.try_emit_string_method(node, callee, args)? {
                    return Ok(());
                }
                // Numeric/Char/Bool primitive methods (`to_float`/`abs`/`sqrt`/…)
                // likewise route by the checker's `recv_kind = "Primitive:Int|…"`
                // before the generic fall-through, which would emit `n.to_float(n)`
                // (no such inherent method on `i64`/`f64`).
                if self.try_emit_numeric_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_list_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_list_functional_method(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_primitive_bridge(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_trait_bound_bridge(node, callee, args)? {
                    return Ok(());
                }
                if self.try_emit_container_method(node, callee, args)? {
                    return Ok(());
                }
                // Desugared instance method call `Call(FieldAccess(recv, m),
                // [recv, ...rest])`: emit `recv.m(rest)` so the receiver flows
                // through Rust's native `&self`, not as a duplicated argument.
                if let Some((recv, method, rest)) =
                    crate::generator::desugared_self_call(callee, args)
                {
                    self.emit_expr(recv)?;
                    let _ = write!(self.buf, ".{}", to_snake_case(&method.name));
                    self.buf.push('(');
                    // A `Self`-operand trait method takes its operand by shared
                    // reference (`a.compare(&b)`) so the caller can keep using
                    // the value afterwards. See `self_operand_methods`.
                    let borrow_operands = self.self_operand_methods.contains(&method.name);
                    for (i, arg) in rest.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        // A `Self`-operand is borrowed; otherwise clone a reused
                        // binding / reused-owner field, or borrow a reused closure
                        // binding (`E0382`/`E0599`). See `emit_call_arg`.
                        self.emit_call_arg(&arg.value, borrow_operands)?;
                    }
                    self.buf.push(')');
                    return Ok(());
                }
                // Pass handler args to effectful function calls.
                let effects_args = if let NodeKind::Identifier { name } = &callee.kind {
                    self.build_effects_call_args_rs(&name.name)
                } else {
                    None
                };
                self.emit_callee_rs(callee)?;
                self.buf.push('(');
                // A `recv.m(b)` whose callee is a `Self`-operand trait method but
                // which is NOT the desugared self-call shape (the receiver isn't
                // duplicated into the arg list, so it falls here) still borrows
                // its operand: `recv.m(&b)`. The leading receiver, if present,
                // is a method receiver (consumed by `recv.m`), so all positional
                // args here are operands.
                let borrow_operands = matches!(
                    &callee.kind,
                    NodeKind::FieldAccess { field, .. }
                        if self.self_operand_methods.contains(&field.name)
                );
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    // A `Self`-operand is borrowed; otherwise clone a reused
                    // match/let binding or a reused-owner field (`filter`'s
                    // `pred(x)` before a later `[x]`), or borrow a reused closure
                    // binding (`E0382`/`E0599`). See `emit_call_arg`.
                    self.emit_call_arg(&arg.value, borrow_operands)?;
                }
                if let Some(ea) = effects_args {
                    if !args.is_empty() {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str(&ea);
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                if self.try_emit_time_method(receiver, &method.name, args)? {
                    return Ok(());
                }
                self.emit_expr(receiver)?;
                let _ = write!(self.buf, ".{}", to_snake_case(&method.name));
                self.buf.push('(');
                let borrow_operands = self.self_operand_methods.contains(&method.name);
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    // A `Self`-operand is borrowed; otherwise clone a reused
                    // binding / reused-owner field, or borrow a reused closure
                    // binding (`E0382`/`E0599`). See `emit_call_arg`.
                    self.emit_call_arg(&arg.value, borrow_operands)?;
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::FieldAccess { object, field } => {
                self.emit_expr(object)?;
                let _ = write!(self.buf, ".{}", to_snake_case(&field.name));
                // Inside a clone-self method, reading a `self` field yields it by
                // value; a `&self` receiver cannot move a non-`Copy` field out, so
                // clone it. The impl carries the matching `T: Clone` bound (when
                // generic) and the record derives `Clone`. Never on an assignment
                // target (`self.cursor = …`): that is a place expression, and
                // `self.cursor.clone() = …` is invalid Rust.
                if self.in_clone_self_method
                    && !self.in_assign_target
                    && matches!(&object.kind, NodeKind::Identifier { name } if name.name == "self")
                {
                    self.buf.push_str(".clone()");
                }
                Ok(())
            }
            NodeKind::Index { object, index } => {
                self.emit_expr(object)?;
                self.buf.push('[');
                self.emit_expr(index)?;
                self.buf.push(']');
                Ok(())
            }
            NodeKind::Lambda { params, body } => {
                let param_strs = self.collect_param_strs(params);
                // A closure returned in tail position of a closure-returning
                // function must `move`-capture (the returned `impl Fn` outlives
                // the frame). See `returning_fn_closure`.
                let move_kw = if self.returning_fn_closure {
                    "move "
                } else {
                    ""
                };
                // A closure *nested* inside the returned one is not itself the
                // return value — disarm so it doesn't also get `move`.
                let prev_ret = std::mem::replace(&mut self.returning_fn_closure, false);
                let prev_tail = std::mem::replace(&mut self.return_closure_tail, false);
                let _ = write!(self.buf, "{move_kw}|{}| ", param_strs.join(", "));
                // A closure used as `.map`/`.filter`/… is `FnMut`/`Fn` — it may
                // run many times, so a by-value pass of a *captured* (non-param)
                // binding moves it out of the closure on the first call (`E0507`).
                // Seed every captured binding the body references for the
                // move-reuse clone path so the call-arg emitter clones it
                // (`category_name(cat)` → `category_name(cat.clone())`). The
                // closure's own params bind fresh values each call and are
                // excluded. Cloning is always sound (all generated types are
                // `Clone`); a captured `Copy` scalar clones harmlessly.
                let mut lambda_params = Vec::new();
                for p in params {
                    if let NodeKind::Param { pattern, .. } = &p.kind {
                        Self::collect_pattern_binding_names(pattern, &mut lambda_params);
                    }
                }
                let lambda_params: std::collections::HashSet<String> = lambda_params
                    .into_iter()
                    .map(|n| to_snake_case(&n))
                    .collect();
                let mut captured = Vec::new();
                Self::collect_identifier_names(body, &mut captured);
                let prev_reused_let = self.reused_let_bindings.clone();
                for name in captured {
                    if !lambda_params.contains(&name) {
                        self.reused_let_bindings.insert(name);
                    }
                }
                let r = self.emit_expr(body);
                self.reused_let_bindings = prev_reused_let;
                self.returning_fn_closure = prev_ret;
                self.return_closure_tail = prev_tail;
                r?;
                Ok(())
            }
            NodeKind::Pipe { left, right } => self.emit_pipe(left, right),
            NodeKind::Compose { left, right } => {
                // `f >> g` → `|x| g(f(x))`. In tail position of a closure-
                // returning function the composed closure captures `f`/`g`, so
                // it must `move` (the returned `impl Fn` outlives the frame).
                let move_kw = if self.returning_fn_closure {
                    "move "
                } else {
                    ""
                };
                let prev_ret = std::mem::replace(&mut self.returning_fn_closure, false);
                let prev_tail = std::mem::replace(&mut self.return_closure_tail, false);
                let _ = write!(self.buf, "{move_kw}|x| ");
                self.emit_expr(right)?;
                self.buf.push('(');
                self.emit_expr(left)?;
                self.buf.push_str("(x))");
                self.returning_fn_closure = prev_ret;
                self.return_closure_tail = prev_tail;
                Ok(())
            }
            NodeKind::Await { expr } => {
                // `await x` where `x` was spawned above becomes
                // `x.await.unwrap()` — `tokio::spawn` returns a `JoinHandle<T>`
                // whose `.await` yields `Result<T, JoinError>`, so we unwrap
                // to restore the original `T` type the user wrote.
                let is_spawned_handle = if let NodeKind::Identifier { name } = &expr.kind {
                    self.task_bound_names.contains(&to_snake_case(&name.name))
                } else {
                    false
                };
                self.emit_expr(expr)?;
                if is_spawned_handle {
                    self.buf.push_str(".await.unwrap()");
                } else {
                    self.buf.push_str(".await");
                }
                Ok(())
            }
            NodeKind::Propagate { expr } => {
                self.emit_expr(expr)?;
                self.buf.push('?');
                Ok(())
            }
            NodeKind::Range { lo, hi, inclusive } => {
                self.emit_expr(lo)?;
                if *inclusive {
                    self.buf.push_str("..=");
                } else {
                    self.buf.push_str("..");
                }
                self.emit_expr(hi)?;
                Ok(())
            }
            NodeKind::RecordConstruct {
                path,
                fields,
                spread,
            } => {
                // A struct-variant construction (`Circle { radius: .. }`) must
                // be qualified `Shape::Circle { .. }`; a plain record keeps its
                // path unqualified.
                let type_name = if let Some(enum_name) = self.variant_enum_qualifier(path) {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    format!("{enum_name}::{variant}")
                } else {
                    path.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("::")
                };
                self.buf.push_str(&type_name);
                self.buf.push_str(" { ");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    let fname = to_snake_case(&f.name.name);
                    if let Some(val) = &f.value {
                        let _ = write!(self.buf, "{fname}: ");
                        self.emit_expr(val)?;
                    } else {
                        self.buf.push_str(&fname);
                    }
                }
                if let Some(sp) = spread {
                    if !fields.is_empty() {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str("..");
                    self.emit_expr(sp)?;
                }
                self.buf.push_str(" }");
                Ok(())
            }
            NodeKind::ListLiteral { elems } => {
                self.buf.push_str("vec![");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                }
                self.buf.push(']');
                Ok(())
            }
            NodeKind::MapLiteral { entries } => {
                if entries.is_empty() {
                    self.buf.push_str("std::collections::HashMap::new()");
                } else {
                    self.buf.push_str("std::collections::HashMap::from([");
                    for (i, entry) in entries.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.buf.push('(');
                        self.emit_expr(&entry.key)?;
                        self.buf.push_str(", ");
                        self.emit_expr(&entry.value)?;
                        self.buf.push(')');
                    }
                    self.buf.push_str("])");
                }
                Ok(())
            }
            NodeKind::SetLiteral { elems } => {
                if elems.is_empty() {
                    self.buf.push_str("std::collections::HashSet::new()");
                } else {
                    self.buf.push_str("std::collections::HashSet::from([");
                    for (i, e) in elems.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_expr(e)?;
                    }
                    self.buf.push_str("])");
                }
                Ok(())
            }
            NodeKind::TupleLiteral { elems } => {
                self.buf.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(e)?;
                }
                if elems.len() == 1 {
                    self.buf.push(',');
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Interpolation { parts } => {
                // `format!("...", args)`
                self.buf.push_str("format!(\"");
                let mut format_args: Vec<String> = Vec::new();
                for part in parts {
                    match part {
                        AirInterpolationPart::Literal(s) => {
                            self.buf.push_str(&escape_format_string(s));
                        }
                        AirInterpolationPart::Expr(expr) => {
                            // A `Vec`/`HashMap`/`HashSet` has no `Display`, so an
                            // interpolated collection value (a collection-typed
                            // binding, or a `.keys()`/list-literal expression)
                            // uses the `Debug` formatter (`{:?}`). See
                            // `collection_bindings` / `expr_is_collection_valued`.
                            if self.expr_interpolates_collection(expr) {
                                self.buf.push_str("{:?}");
                            } else {
                                self.buf.push_str("{}");
                            }
                            let mut sub = RsEmitCtx::new();
                            sub.indent = self.indent;
                            // The sub-context must see the same enum-variant
                            // registry so an interpolated variant construction
                            // (`${label(Red)}`) is qualified `Color::Red` too,
                            // and the `self_operand_methods` set so an
                            // interpolated `${a.compare(b)}` borrows its operand.
                            sub.enum_variants = self.enum_variants.clone();
                            sub.self_operand_methods = self.self_operand_methods.clone();
                            // §10.2/§10.4: an effect op invoked inside an
                            // interpolation (`"at ${now()}"`) must be rewritten
                            // to its handler call (`__clock.now()`) just like one
                            // in statement position. The sub-context therefore
                            // needs the effect-op registry, the in-scope handler
                            // vars, and the fn→effects / composite-effects maps
                            // that drive the rewrite (rs.rs `rewrite_effect_op`).
                            // Without these the op emits bare and rustc fails
                            // with E0425. The other 4 backends emit interpolated
                            // exprs on `self`, so they carry this state already.
                            sub.effect_ops = self.effect_ops.clone();
                            sub.current_handler_vars = self.current_handler_vars.clone();
                            sub.borrowed_handler_effects = self.borrowed_handler_effects.clone();
                            sub.fn_effects = self.fn_effects.clone();
                            sub.composite_effects = self.composite_effects.clone();
                            // Carry the move-reuse clone sets so an interpolated
                            // by-value pass of a reused binding still clones.
                            sub.reused_let_bindings = self.reused_let_bindings.clone();
                            sub.reused_match_bindings = self.reused_match_bindings.clone();
                            sub.emit_expr(expr)?;
                            format_args.push(sub.buf);
                        }
                    }
                }
                self.buf.push('"');
                for arg in format_args {
                    self.buf.push_str(", ");
                    self.buf.push_str(&arg);
                }
                self.buf.push(')');
                Ok(())
            }
            NodeKind::Placeholder => {
                self.buf.push('_');
                Ok(())
            }
            NodeKind::Unreachable => {
                self.buf.push_str("unreachable!()");
                Ok(())
            }
            NodeKind::ResultConstruct { variant, value } => {
                match variant {
                    ResultVariant::Ok => {
                        self.buf.push_str("Ok(");
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("()");
                        }
                        self.buf.push(')');
                    }
                    ResultVariant::Err => {
                        self.buf.push_str("Err(");
                        if let Some(v) = value {
                            self.emit_expr(v)?;
                        } else {
                            self.buf.push_str("()");
                        }
                        self.buf.push(')');
                    }
                }
                Ok(())
            }
            NodeKind::Assign { op, target, value } => {
                // The target is a place expression; suppress the clone-self
                // `self.field` → `self.field.clone()` rewrite while emitting it.
                let prev_assign_target = self.in_assign_target;
                self.in_assign_target = true;
                let target_res = self.emit_expr(target);
                self.in_assign_target = prev_assign_target;
                target_res?;
                let op_str = match op {
                    AssignOp::Assign => " = ",
                    AssignOp::AddAssign => " += ",
                    AssignOp::SubAssign => " -= ",
                    AssignOp::MulAssign => " *= ",
                    AssignOp::DivAssign => " /= ",
                    AssignOp::RemAssign => " %= ",
                };
                self.buf.push_str(op_str);
                self.emit_expr(value)?;
                Ok(())
            }
            NodeKind::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                // If in expression position.
                self.buf.push_str("if ");
                self.emit_expr(condition)?;
                self.buf.push_str(" { ");
                self.emit_block_as_expr(then_block)?;
                self.buf.push_str(" } else { ");
                if let Some(eb) = else_block {
                    self.emit_block_as_expr(eb)?;
                } else {
                    self.buf.push_str("()");
                }
                self.buf.push_str(" }");
                Ok(())
            }
            NodeKind::Block { stmts, tail } => {
                if stmts.is_empty() {
                    if let Some(t) = tail {
                        return self.emit_expr(t);
                    }
                }
                // Block in expression position: `{ stmts; tail }`
                self.buf.push_str("{\n");
                self.indent += 1;
                for s in stmts {
                    self.emit_node(s)?;
                }
                if let Some(t) = tail {
                    self.write_indent();
                    self.emit_expr(t)?;
                    self.buf.push('\n');
                }
                self.indent -= 1;
                self.write_indent();
                self.buf.push('}');
                Ok(())
            }
            NodeKind::Match { scrutinee, arms } => {
                // Match in expression position. Mirror `emit_match`: when any
                // arm matches with a list pattern, match on the scrutinee's
                // `.as_slice()` so the slice patterns match `&[T]` rather than
                // `Vec<T>` (E0529).
                self.buf.push_str("match ");
                if arms.iter().any(Self::arm_matches_list) {
                    self.buf.push('(');
                    self.emit_expr(scrutinee)?;
                    self.buf.push_str(").as_slice()");
                } else if Self::scrutinee_matches_str_literal(arms) {
                    // `String` scrutinee vs `&str` literal arms → `.as_str()`.
                    self.buf.push('(');
                    self.emit_expr(scrutinee)?;
                    self.buf.push_str(").as_str()");
                } else {
                    self.emit_expr(scrutinee)?;
                }
                self.buf.push_str(" {\n");
                self.indent += 1;
                for arm in arms {
                    self.emit_match_arm(arm)?;
                }
                self.indent -= 1;
                self.write_indent();
                self.buf.push('}');
                Ok(())
            }
            // Ownership nodes: direct mapping to Rust.
            NodeKind::Move { expr } => {
                // Move semantics are default in Rust, just emit the expression.
                self.emit_expr(expr)
            }
            NodeKind::Borrow { expr } => {
                self.buf.push('&');
                self.emit_expr(expr)?;
                Ok(())
            }
            NodeKind::MutableBorrow { expr } => {
                self.buf.push_str("&mut ");
                self.emit_expr(expr)?;
                Ok(())
            }
            // Effect operation invocation.
            NodeKind::EffectOp {
                effect,
                operation,
                args,
            } => {
                let effect_name = effect.segments.last().map_or("effect", |s| s.name.as_str());
                let _ = write!(
                    self.buf,
                    "{}.{}",
                    to_snake_case(effect_name),
                    to_snake_case(&operation.name)
                );
                self.buf.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_expr(&arg.value)?;
                }
                self.buf.push(')');
                Ok(())
            }
            // Type expressions in expression context.
            NodeKind::TypeNamed { .. }
            | NodeKind::TypeTuple { .. }
            | NodeKind::TypeFunction { .. }
            | NodeKind::TypeOptional { .. }
            | NodeKind::TypeSelf => {
                self.buf.push_str("/* type */");
                Ok(())
            }
            NodeKind::EffectRef { path } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                self.buf.push_str(&name);
                Ok(())
            }
            NodeKind::Error => {
                self.buf.push_str("/* error */");
                Ok(())
            }
            _ => {
                self.buf.push_str("/* unsupported */");
                Ok(())
            }
        }
    }

    // ── Match ───────────────────────────────────────────────────────────────

    /// Collect the snake-cased binding names a pattern introduces (`Some(x)` →
    /// `["x"]`, `Pair(a, b)` → `["a", "b"]`). Used to seed the move-reuse clone
    /// analysis: a binding the arm body uses more than once must clone on each
    /// by-value use after the first (see `reused_match_bindings`).
    fn collect_pattern_binding_names(pat: &AIRNode, out: &mut Vec<String>) {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => out.push(to_snake_case(&name.name)),
            NodeKind::ConstructorPat { fields, .. } => {
                for e in fields {
                    Self::collect_pattern_binding_names(e, out);
                }
            }
            NodeKind::TuplePat { elems } => {
                for e in elems {
                    Self::collect_pattern_binding_names(e, out);
                }
            }
            NodeKind::ListPat { elems, rest } => {
                for e in elems {
                    Self::collect_pattern_binding_names(e, out);
                }
                if let Some(r) = rest {
                    Self::collect_pattern_binding_names(r, out);
                }
            }
            NodeKind::RecordPat { fields, .. } => {
                for f in fields {
                    if let Some(p) = &f.pattern {
                        Self::collect_pattern_binding_names(p, out);
                    } else {
                        // Shorthand `{ name }` binds `name`.
                        out.push(to_snake_case(&f.name.name));
                    }
                }
            }
            _ => {}
        }
    }

    /// Count how many times the snake-cased identifier `name` is read inside
    /// `node`. A binding read more than once is move-reused (the Rust pattern
    /// binds by value, so the first by-value consumer moves it). Counts every
    /// `Identifier` occurrence; over-counting only ever adds a harmless clone.
    fn count_identifier_uses(node: &AIRNode, name: &str) -> usize {
        struct UseCounter<'a> {
            name: &'a str,
            count: usize,
        }
        impl bock_air::visitor::Visitor for UseCounter<'_> {
            fn visit_node(&mut self, node: &AIRNode) {
                if let NodeKind::Identifier { name } = &node.kind {
                    if to_snake_case(&name.name) == self.name {
                        self.count += 1;
                    }
                }
                bock_air::visitor::walk_node(self, node);
            }
        }
        let mut c = UseCounter { name, count: 0 };
        bock_air::visitor::Visitor::visit_node(&mut c, node);
        c.count
    }

    /// Collect the snake-cased names of every `Identifier` read in `node` (used
    /// to find a closure body's captured bindings — see the `Lambda` arm).
    fn collect_identifier_names(node: &AIRNode, out: &mut Vec<String>) {
        struct NameCollector<'a> {
            out: &'a mut Vec<String>,
        }
        impl bock_air::visitor::Visitor for NameCollector<'_> {
            fn visit_node(&mut self, node: &AIRNode) {
                if let NodeKind::Identifier { name } = &node.kind {
                    self.out.push(to_snake_case(&name.name));
                }
                bock_air::visitor::walk_node(self, node);
            }
        }
        let mut c = NameCollector { out };
        bock_air::visitor::Visitor::visit_node(&mut c, node);
    }

    /// Seed [`Self::reused_let_bindings`] with by-value, non-`Copy` parameters
    /// that the body reads more than once. A Bock parameter is passed by value
    /// (Rust takes ownership); a non-`Copy` value is *moved* by its first
    /// by-value consumer, so a later by-value pass of the same parameter is a
    /// use-after-move (`E0382`) — e.g. `show(op, …)` calling `eval(op, …)` then
    /// `format_expr(op, …)` where `op: Op` is a (non-`Copy`) generated enum. By
    /// registering such parameters here, the call-arg emitter inserts `.clone()`
    /// on each by-value pass (every derived type is `Clone`). Returns the names
    /// added so the caller can restore the set afterward.
    ///
    /// `Copy` scalar parameters (`Int`/`Float`/`Bool`/`Char` → `i64`/`f64`/`bool`
    /// /`char`) and reference-bound `self`-operands are skipped: they are not
    /// moved, and cloning them would be needless (`clippy::clone_on_copy`). The
    /// leading `self` receiver is borrowed, never owned, so it is skipped too.
    fn seed_reused_params(&mut self, params: &[AIRNode], body: &AIRNode) -> Vec<String> {
        let mut added = Vec::new();
        for p in params {
            // Skip the `self` receiver — it lowers to `&self`/`&mut self`, never
            // an owned move.
            if crate::generator::param_binds_self(p).is_some() {
                continue;
            }
            let NodeKind::Param { pattern, ty, .. } = &p.kind else {
                continue;
            };
            // `Copy` scalars are never moved; cloning them is needless noise.
            if ty.as_deref().is_some_and(Self::ast_type_is_copy) {
                continue;
            }
            let NodeKind::BindPat { name, .. } = &pattern.kind else {
                continue;
            };
            let rs_name = to_snake_case(&name.name);
            // A param is move-reused if read by value more than once, OR read
            // even once *inside a loop body* — the loop re-executes that read on
            // each iteration, so the value is moved on the first pass and gone on
            // the second (`category_total`'s `cat` in `for e in …`). A single use
            // outside any loop is a one-shot move and needs no clone.
            if (Self::count_identifier_uses(body, &rs_name) > 1
                || Self::identifier_used_in_loop(body, &rs_name))
                && self.reused_let_bindings.insert(rs_name.clone())
            {
                added.push(rs_name);
            }
        }
        added
    }

    /// True when `name` is read anywhere inside a loop body (`for`/`while`/
    /// `loop`) within `node`. A binding consumed by value there is moved on the
    /// first iteration and unavailable on the next (`E0382`), so it must be
    /// cloned at each by-value pass — see [`Self::seed_reused_params`].
    fn identifier_used_in_loop(node: &AIRNode, name: &str) -> bool {
        struct LoopUseScan<'a> {
            name: &'a str,
            in_loop: usize,
            found: bool,
        }
        impl bock_air::visitor::Visitor for LoopUseScan<'_> {
            fn visit_node(&mut self, node: &AIRNode) {
                match &node.kind {
                    NodeKind::For { body, .. }
                    | NodeKind::While { body, .. }
                    | NodeKind::Loop { body, .. } => {
                        self.in_loop += 1;
                        bock_air::visitor::Visitor::visit_node(self, body);
                        self.in_loop -= 1;
                        // The loop's non-body children (iterable/condition) are
                        // not re-executed per iteration; skip the default walk so
                        // they aren't double-visited or wrongly counted.
                    }
                    NodeKind::Identifier { name } => {
                        if self.in_loop > 0 && to_snake_case(&name.name) == self.name {
                            self.found = true;
                        }
                    }
                    _ => bock_air::visitor::walk_node(self, node),
                }
            }
        }
        let mut s = LoopUseScan {
            name,
            in_loop: 0,
            found: false,
        };
        bock_air::visitor::Visitor::visit_node(&mut s, node);
        s.found
    }

    /// Decide whether a `**` (`BinOp::Pow`) lowers to the float path
    /// (`f64::powf`) or the int path (`i64::pow`). Returns `true` when either
    /// operand is a statically-`Float` expression — currently a `Float` literal
    /// (`b ** 3.0`, `2.0 ** n`). The rust backend has no full local
    /// type-inference environment, so an unannotated `Float`-typed binding on
    /// both sides falls back to the int path; the common cases (`2 ** 10` int,
    /// `x ** 2.0` float) are covered by the literal probe. Choosing the int path
    /// when unsure keeps exact integer precision, matching the go backend.
    fn pow_is_float(left: &AIRNode, right: &AIRNode) -> bool {
        Self::expr_is_float_literal(left) || Self::expr_is_float_literal(right)
    }

    /// True when `node` is (syntactically) a `Float` literal, looking through a
    /// unary negation (`-2.0`). Used to route `**` lowering — see
    /// [`Self::pow_is_float`].
    fn expr_is_float_literal(node: &AIRNode) -> bool {
        match &node.kind {
            NodeKind::Literal {
                lit: Literal::Float(_),
            } => true,
            NodeKind::UnaryOp {
                op: UnaryOp::Neg,
                operand,
            } => Self::expr_is_float_literal(operand),
            _ => false,
        }
    }

    /// True when an AIR type node is a `List`/`Map`/`Set` — a Rust
    /// `Vec`/`HashMap`/`HashSet`, none of which implement `std::fmt::Display`.
    /// A binding of such a type interpolated into a string must use the `Debug`
    /// formatter. See [`Self::collection_bindings`].
    fn type_is_display_collection(ty: &AIRNode) -> bool {
        matches!(
            &ty.kind,
            NodeKind::TypeNamed { path, .. }
                if path.segments.last().is_some_and(|s|
                    matches!(s.name.as_str(), "List" | "Map" | "Set"))
        )
    }

    /// True when `value` (a `let` RHS) syntactically evaluates to a Rust
    /// collection (`Vec`/`HashMap`/`HashSet`) — a list/map/set literal, a list
    /// concatenation (`a + [..]`), or a collection-returning desugared method
    /// (`.keys()`/`.values()`/`.entries()`/`.to_list()`). Used to mark the
    /// binding for `{:?}` interpolation (a collection has no `Display`). A
    /// best-effort syntactic probe — when in doubt it returns `false` (the
    /// binding keeps the default `{}`, which is correct for non-collections).
    fn expr_is_collection_valued(value: &AIRNode) -> bool {
        match &value.kind {
            NodeKind::ListLiteral { .. }
            | NodeKind::MapLiteral { .. }
            | NodeKind::SetLiteral { .. } => true,
            // `a + [..]` / `[..] + b` — a list concatenation is still a `Vec`.
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left,
                right,
            } => Self::expr_is_collection_valued(left) || Self::expr_is_collection_valued(right),
            // A desugared collection-returning method (`map.keys()` etc.). The
            // method name is the trailing field of the callee.
            NodeKind::Call { callee, .. } => {
                if let NodeKind::FieldAccess { field, .. } = &callee.kind {
                    matches!(
                        field.name.as_str(),
                        "keys" | "values" | "entries" | "to_list"
                    )
                } else {
                    false
                }
            }
            NodeKind::MethodCall { method, .. } => matches!(
                method.name.as_str(),
                "keys" | "values" | "entries" | "to_list"
            ),
            _ => false,
        }
    }

    /// True when an interpolated expression evaluates to a Rust collection
    /// (`Vec`/`HashMap`/`HashSet`) — either a bare identifier naming a tracked
    /// [`Self::collection_bindings`] binding (`${keys}`) or a directly
    /// collection-valued expression (`${map.keys()}`, `${[1, 2]}`). Such a value
    /// formats with `{:?}`, not `{}` (collections have no `Display` — E0277).
    fn expr_interpolates_collection(&self, expr: &AIRNode) -> bool {
        if let NodeKind::Identifier { name } = &expr.kind {
            if self
                .collection_bindings
                .contains(&to_snake_case(&name.name))
            {
                return true;
            }
        }
        Self::expr_is_collection_valued(expr)
    }

    /// True when an AIR type node lowers to a `Copy` Rust scalar
    /// (`Int`/`Float`/`Bool`/`Char` → `i64`/`f64`/`bool`/`char`). Such a value is
    /// never moved by a by-value use, so it needs no move-reuse clone.
    /// Conservative: anything else (String, records, enums, containers, optionals,
    /// tuples, functions, generic type vars) is treated as non-`Copy`.
    fn ast_type_is_copy(ty: &AIRNode) -> bool {
        match &ty.kind {
            NodeKind::TypeNamed { path, args } => {
                args.is_empty()
                    && path.segments.last().is_some_and(|s| {
                        matches!(s.name.as_str(), "Int" | "Float" | "Bool" | "Char")
                    })
            }
            _ => false,
        }
    }

    /// True when `arg` is a bare identifier naming a match binding the current
    /// arm reuses ([`Self::reused_match_bindings`]) — a by-value pass of it
    /// after an earlier by-value consumer would move an already-moved value
    /// (`E0382`). The caller emits `<arg>.clone()` instead of `<arg>` for such
    /// args. Bare identifiers only: a non-identifier expression (`f(x)`,
    /// `x + 1`) produces a fresh value with no move hazard.
    fn arg_is_reused_binding(&self, arg: &AIRNode) -> bool {
        match &arg.kind {
            NodeKind::Identifier { name } => {
                let snake = to_snake_case(&name.name);
                self.reused_match_bindings.contains(&snake)
                    || self.reused_let_bindings.contains(&snake)
            }
            _ => false,
        }
    }

    /// Emit an expression in **callee** position, parenthesizing it when its
    /// surface syntax would otherwise swallow the trailing argument list.
    ///
    /// The case that matters is a bare closure callee: `|x| body` followed by
    /// `(arg)` parses in Rust as `|x| (body(arg))` — the call binds to the body,
    /// never invoking the closure. Wrapping it as `(|x| body)(arg)` makes the
    /// call apply to the closure itself. This arises when the AIR compose
    /// desugar (`f >> g` → `(__compose_x) => g(f(__compose_x))`) **nests**:
    /// chained `>>` lowers the inner compose to a `Lambda` (or a `Compose` still
    /// awaiting lowering), which then appears as the callee `f` inside
    /// `f(__compose_x)`. Mirrors the python backend's `emit_callee`.
    fn emit_callee_rs(&mut self, callee: &AIRNode) -> Result<(), CodegenError> {
        if matches!(
            callee.kind,
            NodeKind::Lambda { .. } | NodeKind::Compose { .. }
        ) {
            self.buf.push('(');
            self.emit_expr(callee)?;
            self.buf.push(')');
            Ok(())
        } else {
            self.emit_expr(callee)
        }
    }

    /// True when a by-value call argument must be cloned to keep an earlier
    /// value live (`E0382`). Extends [`Self::arg_is_reused_binding`] (the bare
    /// reused-binding case) with the **record-field** case: passing
    /// `owner.field` by value moves the field out of `owner`, partially moving
    /// it; if `owner` is itself a move-reused binding (read again afterward — a
    /// later `owner.other`, `owner.method()`, or the same `owner.field` again),
    /// that later read is a use-after-(partial-)move. Cloning the field at the
    /// pass site leaves `owner` intact (every generated record derives `Clone`),
    /// mirroring [`Self::iterable_is_reused`] for `for`-iterables. A non-reused
    /// owner (a fresh value, a one-shot local) needs no clone: the single move is
    /// fine. Only a one-level `<ident>.<field>` is handled; deeper chains
    /// (`a.b.c`) are rare in v1 and fall through to the no-clone path.
    fn arg_needs_clone(&self, arg: &AIRNode) -> bool {
        if self.arg_is_reused_binding(arg) {
            return true;
        }
        if let NodeKind::FieldAccess { object, .. } = &arg.kind {
            // `owner.field` clones when `owner` is a reused binding: the field
            // move would partially move `owner`, breaking a later read of it.
            return self.arg_is_reused_binding(object);
        }
        false
    }

    /// True when `arg` is a bare identifier naming a function/closure-valued
    /// binding ([`Self::fn_typed_bindings`]) that is reused
    /// ([`Self::reused_let_bindings`]). Such a binding holds an `impl Fn` opaque
    /// value, which is **not** `Clone` (E0599) — so a move-reuse pass must
    /// *borrow* it (`&f`) rather than clone it. `&F` satisfies an `impl Fn`
    /// parameter when `F: Fn`, and the borrow leaves the binding live for the
    /// next pass.
    fn arg_is_reused_fn_binding(&self, arg: &AIRNode) -> bool {
        if let NodeKind::Identifier { name } = &arg.kind {
            let snake = to_snake_case(&name.name);
            return self.fn_typed_bindings.contains(&snake)
                && self.reused_let_bindings.contains(&snake);
        }
        false
    }

    /// Emit a single positional call argument, applying the move-reuse fixups a
    /// by-value Rust call needs to honour Bock's value semantics:
    ///
    /// - `borrow` (the `Self`-operand-trait case): pass by shared reference
    ///   (`&arg`) so the caller can keep using the value.
    /// - a reused function/closure binding: borrow it (`&f`) — an `impl Fn` is
    ///   not `Clone`, but `&F: Fn` ([`Self::arg_is_reused_fn_binding`]).
    /// - a reused non-`Copy` binding or a record field of a reused owner: clone
    ///   it (`arg.clone()`) so a later use stays live
    ///   ([`Self::arg_needs_clone`]).
    fn emit_call_arg(&mut self, arg: &AIRNode, borrow: bool) -> Result<(), CodegenError> {
        if borrow {
            self.buf.push('&');
            self.emit_expr(arg)?;
            return Ok(());
        }
        if self.arg_is_reused_fn_binding(arg) {
            self.buf.push('&');
            self.emit_expr(arg)?;
            return Ok(());
        }
        let clone_reused = self.arg_needs_clone(arg);
        self.emit_expr(arg)?;
        if clone_reused {
            self.buf.push_str(".clone()");
        }
        Ok(())
    }

    /// True when a `for` iterable should be cloned to avoid moving (or partially
    /// moving) a binding that is reused after the loop. Covers a bare reused
    /// binding (`for n in nodes` with `nodes` used again — [`Self::arg_is_reused_binding`])
    /// *and* a field access of one (`for row in dataset.rows` with `dataset`
    /// reused: iterating the field partially moves the owner, so a later use of
    /// `dataset` is `E0382`). A field access is cloned when its root binding is
    /// reused.
    fn iterable_is_reused(&self, iterable: &AIRNode) -> bool {
        if self.arg_is_reused_binding(iterable) {
            return true;
        }
        if let NodeKind::FieldAccess { object, .. } = &iterable.kind {
            return self.arg_is_reused_binding(object);
        }
        false
    }

    fn emit_match(&mut self, scrutinee: &AIRNode, arms: &[AIRNode]) -> Result<(), CodegenError> {
        let ind = self.indent_str();
        let _ = write!(self.buf, "{ind}match ");
        // Bock list/array values are `Vec<T>` in this backend, but Rust slice
        // patterns (`[]`, `[head, ..tail]`) only match `[T]`/`&[T]`, not
        // `Vec<T>` (E0529). When any arm matches with a list pattern, match on
        // the scrutinee's `.as_slice()` (`&[T]`): default binding modes then
        // bind elements by shared reference, and a `rest @ ..` tail binds to a
        // sized `&[T]` (a by-value `[T]` tail would be unsized — E0277).
        let slice_match = arms.iter().any(Self::arm_matches_list);
        if slice_match {
            self.buf.push('(');
            self.emit_expr(scrutinee)?;
            self.buf.push_str(").as_slice()");
        } else if Self::scrutinee_matches_str_literal(arms) {
            // `String` scrutinee vs `&str` literal arms → match on `.as_str()`.
            self.buf.push('(');
            self.emit_expr(scrutinee)?;
            self.buf.push_str(").as_str()");
        } else {
            self.emit_expr(scrutinee)?;
        }
        self.buf.push_str(" {\n");
        self.indent += 1;
        for arm in arms {
            self.emit_match_arm(arm)?;
        }
        self.indent -= 1;
        self.writeln("}");
        Ok(())
    }

    /// Whether a match arm's pattern is (or, under `|`/guard, contains) a list
    /// pattern — the signal to match on the scrutinee's `.as_slice()`. See
    /// [`Self::emit_match`].
    fn arm_matches_list(arm: &AIRNode) -> bool {
        if let NodeKind::MatchArm { pattern, .. } = &arm.kind {
            Self::pattern_is_list(pattern)
        } else {
            false
        }
    }

    /// Whether `pat` is a list pattern, looking through `|`-alternatives and a
    /// trailing pattern guard.
    fn pattern_is_list(pat: &AIRNode) -> bool {
        match &pat.kind {
            NodeKind::ListPat { .. } => true,
            NodeKind::OrPat { alternatives } => alternatives.iter().any(Self::pattern_is_list),
            NodeKind::GuardPat { pattern, .. } => Self::pattern_is_list(pattern),
            _ => false,
        }
    }

    /// Whether a match arm's pattern is (or, under `|`/guard, contains) a string
    /// literal pattern — the signal to match on the scrutinee's `.as_str()`. See
    /// [`Self::scrutinee_matches_str_literal`].
    fn arm_matches_str_literal(arm: &AIRNode) -> bool {
        if let NodeKind::MatchArm { pattern, .. } = &arm.kind {
            Self::pattern_is_str_literal(pattern)
        } else {
            false
        }
    }

    /// Whether `pat` is a string-literal pattern (`"foo"`), looking through
    /// `|`-alternatives and a trailing pattern guard.
    fn pattern_is_str_literal(pat: &AIRNode) -> bool {
        match &pat.kind {
            NodeKind::LiteralPat {
                lit: Literal::String(_),
            } => true,
            NodeKind::OrPat { alternatives } => {
                alternatives.iter().any(Self::pattern_is_str_literal)
            }
            NodeKind::GuardPat { pattern, .. } => Self::pattern_is_str_literal(pattern),
            _ => false,
        }
    }

    /// Whether an arm's pattern binds the whole scrutinee by name at top level
    /// (`other => …`), looking through `|`-alternatives and a trailing guard. Such
    /// a binding would change type from owned `String` to `&str` if the scrutinee
    /// were `.as_str()`-wrapped, so its presence suppresses the wrap (see
    /// [`Self::scrutinee_matches_str_literal`]).
    fn arm_binds_scrutinee(arm: &AIRNode) -> bool {
        if let NodeKind::MatchArm { pattern, .. } = &arm.kind {
            Self::pattern_binds_scrutinee(pattern)
        } else {
            false
        }
    }

    /// Whether `pat` is a top-level binding pattern (`other`), looking through
    /// `|`-alternatives and a trailing pattern guard. A wildcard (`_`) does not
    /// bind, so it is not counted.
    fn pattern_binds_scrutinee(pat: &AIRNode) -> bool {
        match &pat.kind {
            NodeKind::BindPat { .. } => true,
            NodeKind::OrPat { alternatives } => {
                alternatives.iter().any(Self::pattern_binds_scrutinee)
            }
            NodeKind::GuardPat { pattern, .. } => Self::pattern_binds_scrutinee(pattern),
            _ => false,
        }
    }

    /// Whether this match should match on `(scrutinee).as_str()` rather than the
    /// scrutinee directly. Bock's only string type is `String`, which lowers to
    /// an owned Rust `String`; a string-literal pattern (`"foo"`) has type `&str`,
    /// so matching a `String` scrutinee against it is `String` vs `&str` (E0308).
    /// Matching `(s).as_str()` (a `&str`) against the `&str` literals lines the two
    /// up.
    ///
    /// Fires only when (a) some arm carries a string-literal pattern, (b) no arm
    /// carries a list pattern (those take `.as_slice()`, and string/list arms
    /// cannot coexist on one scrutinee anyway), and (c) no arm binds the whole
    /// scrutinee by name (`other => …`) — wrapping would retype such a binding from
    /// owned `String` to `&str`, so it is left untouched. A scrutinee with only
    /// bind/wildcard/constructor patterns (no string literal) is likewise left
    /// alone, preserving an owned-`String` binding's type.
    fn scrutinee_matches_str_literal(arms: &[AIRNode]) -> bool {
        arms.iter().any(Self::arm_matches_str_literal)
            && !arms.iter().any(Self::arm_matches_list)
            && !arms.iter().any(Self::arm_binds_scrutinee)
    }

    fn emit_match_arm(&mut self, arm: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::MatchArm {
            pattern,
            guard,
            body,
        } = &arm.kind
        {
            // Seed the move-reuse clone set for this arm: any pattern binding
            // the body reads more than once is moved by its first by-value
            // consumer, so later by-value uses must `.clone()` (`E0382`). Scoped
            // to the arm (saved/restored) so it never leaks to a sibling/outer
            // arm. See `reused_match_bindings`.
            let prev_reused = self.reused_match_bindings.clone();
            let mut bound = Vec::new();
            Self::collect_pattern_binding_names(pattern, &mut bound);
            for name in bound {
                if Self::count_identifier_uses(body, &name) > 1 {
                    self.reused_match_bindings.insert(name);
                }
            }
            let ind = self.indent_str();
            let _ = write!(self.buf, "{ind}");
            self.emit_pattern(pattern)?;
            if let Some(g) = guard {
                self.buf.push_str(" if ");
                self.emit_expr(g)?;
            }
            self.buf.push_str(" => ");
            // A statement-bodied arm (`break`/`continue`/`return`/assignment,
            // or a block whose tail is one) has no value. Rust `match` arms
            // accept statements directly, so route such a body through the
            // statement emitter inside a `{ }` block.
            if crate::generator::arm_body_is_statement(body) {
                self.buf.push_str("{\n");
                self.indent += 1;
                if let NodeKind::Block { .. } = &body.kind {
                    self.emit_block_body(body)?;
                } else {
                    self.emit_stmt(body)?;
                }
                self.indent -= 1;
                self.writeln("}");
                self.reused_match_bindings = prev_reused;
                return Ok(());
            }
            // Single-expression body → inline; otherwise block.
            if let NodeKind::Block { stmts, tail } = &body.kind {
                if stmts.is_empty() {
                    if let Some(t) = tail {
                        self.emit_expr(t)?;
                        self.buf.push_str(",\n");
                        self.reused_match_bindings = prev_reused;
                        return Ok(());
                    }
                }
                self.buf.push_str("{\n");
                self.indent += 1;
                self.emit_block_body(body)?;
                self.indent -= 1;
                self.writeln("}");
            } else {
                self.emit_expr(body)?;
                self.buf.push_str(",\n");
            }
            self.reused_match_bindings = prev_reused;
        }
        Ok(())
    }

    fn emit_pattern(&mut self, pat: &AIRNode) -> Result<(), CodegenError> {
        match &pat.kind {
            NodeKind::WildcardPat => {
                self.buf.push('_');
            }
            NodeKind::BindPat { name, is_mut } => {
                if *is_mut {
                    self.buf.push_str("mut ");
                }
                self.buf.push_str(&to_snake_case(&name.name));
            }
            NodeKind::LiteralPat { lit } => match lit {
                Literal::Int(s) => {
                    self.buf.push_str(s);
                    self.buf.push_str("_i64");
                }
                Literal::Float(s) => self.buf.push_str(s),
                Literal::Bool(b) => self.buf.push_str(if *b { "true" } else { "false" }),
                Literal::Char(s) => {
                    self.buf.push('\'');
                    self.buf.push_str(s);
                    self.buf.push('\'');
                }
                Literal::String(s) => {
                    self.buf.push('"');
                    self.buf.push_str(&escape_rs_string(s));
                    self.buf.push('"');
                }
                Literal::Unit => self.buf.push_str("()"),
            },
            NodeKind::ConstructorPat { path, fields } => {
                // Prelude `Ordering` variant patterns match Rust's native
                // `std::cmp::Ordering` (the construction side maps the same way)
                // — UNLESS the real `core.compare.Ordering` enum is reachable, in
                // which case the user enum (`Ordering::Less`) is matched via the
                // qualifier path below.
                if let Some(variant) = path
                    .segments
                    .last()
                    .and_then(|s| crate::generator::ordering_variant(&s.name))
                {
                    if fields.is_empty() && !self.ordering_enum_reachable() {
                        let _ = write!(self.buf, "std::cmp::Ordering::{variant}");
                        return Ok(());
                    }
                }
                // Qualify a user enum-variant pattern `Enum::Variant`; built-in
                // and non-variant paths keep their original `::`-joined form.
                let variant_name = if let Some(enum_name) = self.variant_enum_qualifier(path) {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    format!("{enum_name}::{variant}")
                } else {
                    path.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("::")
                };
                if fields.is_empty() {
                    self.buf.push_str(&variant_name);
                } else {
                    let _ = write!(self.buf, "{variant_name}(");
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            self.buf.push_str(", ");
                        }
                        self.emit_pattern(f)?;
                    }
                    self.buf.push(')');
                }
            }
            NodeKind::RecordPat { path, fields, rest } => {
                let type_name = if let Some(enum_name) = self.variant_enum_qualifier(path) {
                    let variant = path.segments.last().map_or("", |s| s.name.as_str());
                    format!("{enum_name}::{variant}")
                } else {
                    path.segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("::")
                };
                let _ = write!(self.buf, "{type_name} {{ ");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    let field_name = to_snake_case(&f.name.name);
                    if let Some(pat) = &f.pattern {
                        let _ = write!(self.buf, "{field_name}: ");
                        self.emit_pattern(pat)?;
                    } else {
                        self.buf.push_str(&field_name);
                    }
                }
                if *rest {
                    if !fields.is_empty() {
                        self.buf.push_str(", ");
                    }
                    self.buf.push_str("..");
                }
                self.buf.push_str(" }");
            }
            NodeKind::TuplePat { elems } => {
                self.buf.push('(');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_pattern(e)?;
                }
                self.buf.push(')');
            }
            NodeKind::ListPat { elems, rest } => {
                self.buf.push('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    self.emit_pattern(e)?;
                }
                if let Some(r) = rest {
                    if !elems.is_empty() {
                        self.buf.push_str(", ");
                    }
                    self.emit_pattern(r)?;
                    self.buf.push_str(" @ ..");
                }
                self.buf.push(']');
            }
            NodeKind::OrPat { alternatives } => {
                for (i, p) in alternatives.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(" | ");
                    }
                    self.emit_pattern(p)?;
                }
            }
            NodeKind::GuardPat { pattern, guard } => {
                self.emit_pattern(pattern)?;
                self.buf.push_str(" if ");
                self.emit_expr(guard)?;
            }
            NodeKind::RangePat { lo, hi, inclusive } => {
                self.emit_pattern(lo)?;
                if *inclusive {
                    self.buf.push_str("..=");
                } else {
                    self.buf.push_str("..");
                }
                self.emit_pattern(hi)?;
            }
            NodeKind::RestPat => {
                self.buf.push_str("..");
            }
            _ => {
                self.buf.push('_');
            }
        }
        Ok(())
    }

    // ── Pipe operator ───────────────────────────────────────────────────────

    fn emit_pipe(&mut self, left: &AIRNode, right: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Call { callee, args, .. } = &right.kind {
            let has_placeholder = args
                .iter()
                .any(|a| matches!(a.value.kind, NodeKind::Placeholder));
            if has_placeholder {
                self.emit_expr(callee)?;
                self.buf.push('(');
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.buf.push_str(", ");
                    }
                    if matches!(arg.value.kind, NodeKind::Placeholder) {
                        self.emit_expr(left)?;
                    } else {
                        self.emit_expr(&arg.value)?;
                    }
                }
                self.buf.push(')');
                return Ok(());
            }
        }
        // `x |> (|v| …)` pipes into a closure: parenthesize the closure callee
        // so the `(left)` call applies to it, not to its body. See
        // `emit_callee_rs`.
        self.emit_callee_rs(right)?;
        self.buf.push('(');
        self.emit_expr(left)?;
        self.buf.push(')');
        Ok(())
    }

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn emit_block_body(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            if stmts.is_empty() && tail.is_none() {
                // Empty block body.
                return Ok(());
            }
            // Concurrent-pattern detection: names bound in this block whose
            // Call RHS should be scheduled via `tokio::spawn` because the
            // same name is later `await`ed in the same block. Rust futures
            // are lazy, so without spawning, sequential `.await` calls on
            // each binding would serialise the work.
            let task_bindings = Self::collect_task_bindings(stmts);
            let prev = std::mem::replace(&mut self.task_bound_names, task_bindings);
            // Seed the move-reuse clone set for this block's `let` bindings: a
            // non-`Copy` binding read by value more than once is moved by its
            // first by-value consumer, so later free-fn arg passes must clone
            // (`E0382`). Unioned into (not replacing) any outer-block set so a
            // reused binding from an enclosing block stays cloned in nested
            // blocks; saved/restored so the additions never leak outward.
            let prev_reused_let = self.reused_let_bindings.clone();
            // Track which `let` bindings hold a Rust collection so an
            // interpolation of one formats with `{:?}` (a `Vec`/`HashMap`/
            // `HashSet` has no `Display`). See `collection_bindings`.
            let prev_collection = self.collection_bindings.clone();
            // Track which `let` bindings hold a function/closure (`impl Fn`) so a
            // move-reuse of one is *borrowed* (`&f`) rather than `.clone()`d — an
            // `impl Fn` opaque type is not `Clone` (E0599). See
            // `fn_typed_bindings`.
            let prev_fn_typed = self.fn_typed_bindings.clone();
            for s in stmts {
                if let NodeKind::LetBinding {
                    pattern, value, ty, ..
                } = &s.kind
                {
                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                        let rs_name = to_snake_case(&name.name);
                        if Self::count_identifier_uses(node, &rs_name) > 1 {
                            self.reused_let_bindings.insert(rs_name.clone());
                        }
                        if ty.as_deref().is_some_and(Self::type_is_display_collection)
                            || Self::expr_is_collection_valued(value)
                        {
                            self.collection_bindings.insert(rs_name.clone());
                        }
                        if ty
                            .as_deref()
                            .is_some_and(|t| matches!(&t.kind, NodeKind::TypeFunction { .. }))
                            || self.rhs_is_fn_valued(value)
                        {
                            self.fn_typed_bindings.insert(rs_name);
                        }
                    }
                }
            }
            for s in stmts {
                self.emit_node(s)?;
            }
            self.reused_let_bindings = prev_reused_let;
            self.collection_bindings = prev_collection;
            self.fn_typed_bindings = prev_fn_typed;
            self.task_bound_names = prev;
            if let Some(t) = tail {
                // A statement tail (`return`/`break`/`continue`/assignment) is
                // emitted via the statement emitter — `emit_expr` has no arm
                // for these control-flow nodes and would emit
                // `/* unsupported */`.
                if crate::generator::node_is_statement(t) {
                    self.emit_stmt(t)?;
                    return Ok(());
                }
                // Tail expression without semicolon (Rust implicit return).
                self.write_indent();
                let prev = self.returning_fn_closure;
                self.returning_fn_closure = self.return_closure_tail;
                let r = self.emit_expr(t);
                self.returning_fn_closure = prev;
                r?;
                self.buf.push('\n');
            }
        } else if crate::generator::node_is_statement(node) {
            self.emit_stmt(node)?;
        } else {
            // Single expression as body (implicit return).
            self.write_indent();
            let prev = self.returning_fn_closure;
            self.returning_fn_closure = self.return_closure_tail;
            let r = self.emit_expr(node);
            self.returning_fn_closure = prev;
            r?;
            self.buf.push('\n');
        }
        Ok(())
    }

    /// Emit a `@test` function body (S7), lowering `expect(...)` assertion
    /// chains to Rust `assert!` / `assert_eq!` and falling back to the normal
    /// statement emitter for any other statement (`let`, helper calls, …).
    fn emit_test_body(&mut self, body: &AIRNode) -> Result<(), CodegenError> {
        let emit_one = |this: &mut Self, stmt: &AIRNode| -> Result<(), CodegenError> {
            if let Some((assertion, actual, expected)) = crate::generator::classify_assertion(stmt)
            {
                let a = this.expr_to_string(actual)?;
                let line = match assertion {
                    crate::generator::TestAssertion::Equal => {
                        let e = match expected {
                            Some(e) => this.expr_to_string(e)?,
                            None => "()".to_string(),
                        };
                        format!("assert_eq!({a}, {e});")
                    }
                    crate::generator::TestAssertion::BeTrue => format!("assert!({a});"),
                    crate::generator::TestAssertion::BeFalse => format!("assert!(!({a}));"),
                    crate::generator::TestAssertion::BeSome => format!("assert!(({a}).is_some());"),
                    crate::generator::TestAssertion::BeNone => format!("assert!(({a}).is_none());"),
                    crate::generator::TestAssertion::BeOk => format!("assert!(({a}).is_ok());"),
                    crate::generator::TestAssertion::BeErr => format!("assert!(({a}).is_err());"),
                };
                this.writeln(&line);
                Ok(())
            } else {
                this.emit_node(stmt)
            }
        };
        if let NodeKind::Block { stmts, tail } = &body.kind {
            for s in stmts {
                emit_one(self, s)?;
            }
            if let Some(t) = tail {
                emit_one(self, t)?;
            }
        } else {
            emit_one(self, body)?;
        }
        Ok(())
    }

    /// Scan a sequence of block statements and return the set of bound names
    /// that are later `await`ed as bare identifiers within the same block.
    /// The caller wraps those LetBindings' Call values in `tokio::spawn`.
    ///
    /// Only direct `let name = call(...)` bindings qualify. Non-call RHS are
    /// skipped (nothing to spawn). The binding must be awaited in the same
    /// flat block — nested scopes are ignored because we can't prove the
    /// binding is still live once control leaves the block.
    fn collect_task_bindings(stmts: &[AIRNode]) -> std::collections::HashSet<String> {
        let mut awaited: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in stmts {
            Self::collect_awaited_identifiers(s, &mut awaited);
        }
        let mut out = std::collections::HashSet::new();
        for s in stmts {
            if let NodeKind::LetBinding { pattern, value, .. } = &s.kind {
                if let NodeKind::BindPat { name, .. } = &pattern.kind {
                    let rs_name = to_snake_case(&name.name);
                    if matches!(&value.kind, NodeKind::Call { .. }) && awaited.contains(&rs_name) {
                        out.insert(rs_name);
                    }
                }
            }
        }
        out
    }

    /// Walk an AIR subtree and record every `await name` where `name` is a
    /// bare identifier. Nested function / lambda bodies are not descended —
    /// an inner closure awaiting the name doesn't imply the outer block
    /// wants a task.
    fn collect_awaited_identifiers(node: &AIRNode, out: &mut std::collections::HashSet<String>) {
        match &node.kind {
            NodeKind::Await { expr } => {
                if let NodeKind::Identifier { name } = &expr.kind {
                    out.insert(to_snake_case(&name.name));
                }
                Self::collect_awaited_identifiers(expr, out);
            }
            NodeKind::Lambda { .. } | NodeKind::FnDecl { .. } => {
                // Don't cross function boundaries.
            }
            NodeKind::Block { stmts, tail } => {
                for s in stmts {
                    Self::collect_awaited_identifiers(s, out);
                }
                if let Some(t) = tail {
                    Self::collect_awaited_identifiers(t, out);
                }
            }
            NodeKind::LetBinding { value, .. } => {
                Self::collect_awaited_identifiers(value, out);
            }
            NodeKind::Call { callee, args, .. } => {
                Self::collect_awaited_identifiers(callee, out);
                for a in args {
                    Self::collect_awaited_identifiers(&a.value, out);
                }
            }
            NodeKind::MethodCall { receiver, args, .. } => {
                Self::collect_awaited_identifiers(receiver, out);
                for a in args {
                    Self::collect_awaited_identifiers(&a.value, out);
                }
            }
            NodeKind::BinaryOp { left, right, .. } => {
                Self::collect_awaited_identifiers(left, out);
                Self::collect_awaited_identifiers(right, out);
            }
            NodeKind::UnaryOp { operand, .. } => {
                Self::collect_awaited_identifiers(operand, out);
            }
            NodeKind::If {
                condition,
                then_block,
                else_block,
                ..
            } => {
                Self::collect_awaited_identifiers(condition, out);
                Self::collect_awaited_identifiers(then_block, out);
                if let Some(e) = else_block {
                    Self::collect_awaited_identifiers(e, out);
                }
            }
            NodeKind::While { condition, body } => {
                Self::collect_awaited_identifiers(condition, out);
                Self::collect_awaited_identifiers(body, out);
            }
            NodeKind::For { iterable, body, .. } => {
                Self::collect_awaited_identifiers(iterable, out);
                Self::collect_awaited_identifiers(body, out);
            }
            NodeKind::Return { value: Some(v) } | NodeKind::Break { value: Some(v) } => {
                Self::collect_awaited_identifiers(v, out);
            }
            NodeKind::Assign { value, .. } => {
                Self::collect_awaited_identifiers(value, out);
            }
            NodeKind::TupleLiteral { elems } | NodeKind::ListLiteral { elems } => {
                for e in elems {
                    Self::collect_awaited_identifiers(e, out);
                }
            }
            _ => {}
        }
    }

    fn emit_block_as_expr(&mut self, node: &AIRNode) -> Result<(), CodegenError> {
        if let NodeKind::Block { stmts, tail } = &node.kind {
            if stmts.is_empty() {
                if let Some(t) = tail {
                    return self.emit_expr(t);
                }
            }
        }
        self.emit_expr(node)
    }

    fn pattern_to_binding_name(&self, pat: &AIRNode) -> String {
        match &pat.kind {
            NodeKind::BindPat { name, .. } => to_snake_case(&name.name),
            NodeKind::WildcardPat => "_".into(),
            NodeKind::TuplePat { elems } => {
                format!(
                    "({})",
                    elems
                        .iter()
                        .map(|e| self.pattern_to_binding_name(e))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            _ => "_".into(),
        }
    }

    fn pattern_to_rs_binding(&self, pat: &AIRNode) -> String {
        self.pattern_to_binding_name(pat)
    }

    fn type_expr_to_string(&mut self, node: &AIRNode) -> String {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let name = path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                if args.is_empty() {
                    name
                } else {
                    let arg_strs: Vec<String> = args.iter().map(|a| self.type_to_rs(a)).collect();
                    format!("{name}<{}>", arg_strs.join(", "))
                }
            }
            NodeKind::Identifier { name } => name.name.clone(),
            _ => "Unknown".into(),
        }
    }
}

// ─── Utility functions ───────────────────────────────────────────────────────

/// Visibility keyword.
fn vis_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "pub ",
        Visibility::Private => "",
        Visibility::Internal => "pub(crate) ",
    }
}

/// If `node` is a record construction, return the fully-qualified type path
/// used in the constructor. Used by module-level `handle` emission to pick a
/// concrete type annotation for the synthesised `const`.
fn record_construct_type(node: &AIRNode) -> Option<String> {
    if let NodeKind::RecordConstruct { path, .. } = &node.kind {
        let joined = path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        Some(joined)
    } else {
        None
    }
}

/// Emit a Bock identifier as a Rust identifier — PascalCase names are
/// preserved verbatim (they are types, enum variants, or tuple-struct
/// constructors), while everything else is converted to snake_case.
fn identifier_to_rs(s: &str) -> String {
    if s.chars().next().is_some_and(char::is_uppercase) {
        s.to_string()
    } else {
        to_snake_case(s)
    }
}

/// Returns true if `name` is the identifier of a Duration or Instant instance
/// method. Used to recognise `d.as_millis()` / `i.elapsed()` calls during codegen.
fn is_time_method_name(name: &str) -> bool {
    matches!(
        name,
        "as_nanos"
            | "as_millis"
            | "as_seconds"
            | "is_zero"
            | "is_negative"
            | "abs"
            | "elapsed"
            | "duration_since"
    )
}

/// Convert a `PascalCase` or `camelCase` name to `snake_case`.
fn to_snake_case(s: &str) -> String {
    if s.is_empty() || s == "_" {
        return s.to_string();
    }
    if s.contains('_') && !s.chars().any(|c| c.is_uppercase()) {
        return s.to_string();
    }
    if !s.chars().any(|c| c.is_uppercase()) {
        return s.to_string();
    }
    if s.len() == 1 {
        return s.to_lowercase();
    }

    let mut result = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();

    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_uppercase() {
            let prev_is_upper = i > 0 && chars[i - 1].is_uppercase();
            let prev_is_underscore = i > 0 && chars[i - 1] == '_';
            let next_is_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
            if i > 0 && !prev_is_underscore && (!prev_is_upper || next_is_lower) {
                result.push('_');
            }
            result.push(
                ch.to_lowercase()
                    .next()
                    .expect("lowercase yields at least one char"),
            );
        } else {
            result.push(ch);
        }
    }
    result
}

/// Convert a name to `UPPER_SNAKE_CASE` for constants.
fn to_upper_snake_case(s: &str) -> String {
    to_snake_case(s).to_uppercase()
}

/// Escape special characters in a Rust string literal.
fn escape_rs_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape special characters in a `format!()` format string.
fn escape_format_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            _ => out.push(ch),
        }
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AirArg, AirMapEntry, AirRecordField};
    use bock_ast::{
        GenericParam, Ident, ImportItems, ImportedName, ModulePath, RecordDeclField, TypePath,
    };
    use bock_errors::{FileId, Span};

    fn span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.to_string(),
            span: span(),
        }
    }

    fn type_path(segments: &[&str]) -> TypePath {
        TypePath {
            segments: segments.iter().map(|s| ident(s)).collect(),
            span: span(),
        }
    }

    fn mod_path(segments: &[&str]) -> ModulePath {
        ModulePath {
            segments: segments.iter().map(|s| ident(s)).collect(),
            span: span(),
        }
    }

    fn imported_name(name: &str) -> ImportedName {
        ImportedName {
            span: span(),
            name: ident(name),
            alias: None,
        }
    }

    fn record_field(name: &str, ty_name: &str) -> RecordDeclField {
        RecordDeclField {
            id: 0,
            span: span(),
            name: ident(name),
            ty: TypeExpr::Named {
                id: 0,
                path: type_path(&[ty_name]),
                args: vec![],
                span: span(),
            },
            default: None,
        }
    }

    fn node(id: u32, kind: NodeKind) -> AIRNode {
        AIRNode::new(id, span(), kind)
    }

    fn int_lit(id: u32, val: &str) -> AIRNode {
        node(
            id,
            NodeKind::Literal {
                lit: Literal::Int(val.into()),
            },
        )
    }

    fn str_lit(id: u32, val: &str) -> AIRNode {
        node(
            id,
            NodeKind::Literal {
                lit: Literal::String(val.into()),
            },
        )
    }

    fn bool_lit(id: u32, val: bool) -> AIRNode {
        node(
            id,
            NodeKind::Literal {
                lit: Literal::Bool(val),
            },
        )
    }

    fn id_node(id: u32, name: &str) -> AIRNode {
        node(id, NodeKind::Identifier { name: ident(name) })
    }

    fn bind_pat(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::BindPat {
                name: ident(name),
                is_mut: false,
            },
        )
    }

    fn typed_param_node(id: u32, name: &str, ty_name: &str) -> AIRNode {
        node(
            id,
            NodeKind::Param {
                pattern: Box::new(bind_pat(id + 100, name)),
                ty: Some(Box::new(node(
                    id + 200,
                    NodeKind::TypeNamed {
                        path: type_path(&[ty_name]),
                        args: vec![],
                    },
                ))),
                default: None,
            },
        )
    }

    fn block(id: u32, stmts: Vec<AIRNode>, tail: Option<AIRNode>) -> AIRNode {
        node(
            id,
            NodeKind::Block {
                stmts,
                tail: tail.map(Box::new),
            },
        )
    }

    fn module(imports: Vec<AIRNode>, items: Vec<AIRNode>) -> AIRNode {
        node(
            0,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports,
                items,
            },
        )
    }

    fn gen(module: &AIRNode) -> String {
        let gen = RsGenerator::new();
        let result = gen.generate_module(module).unwrap();
        result.files[0].content.clone()
    }

    /// Run `rustc --edition 2021 --crate-type lib` to validate syntax.
    fn check_rs_syntax(code: &str) -> bool {
        use std::io::Write;
        use std::process::Command;
        let id = std::thread::current().id();
        let dir = std::env::temp_dir().join(format!("bock_rs_test_{id:?}"));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_output.rs");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(code.as_bytes()).unwrap();
        }
        let output = Command::new("rustc")
            .args([
                "--edition",
                "2021",
                "--crate-type",
                "lib",
                "-o",
                dir.join("test_output.rlib").to_str().unwrap(),
            ])
            .arg(&path)
            .stderr(std::process::Stdio::piped())
            .output();
        match output {
            Ok(o) => {
                if !o.status.success() {
                    eprintln!("rustc stderr: {}", String::from_utf8_lossy(&o.stderr));
                }
                o.status.success()
            }
            Err(_) => false,
        }
    }

    // ── Basic tests ─────────────────────────────────────────────────────────

    #[test]
    fn implements_code_generator_trait() {
        let gen = RsGenerator::new();
        assert_eq!(gen.target().id, "rust");
    }

    #[test]
    fn empty_module() {
        let m = module(vec![], vec![]);
        let out = gen(&m);
        assert_eq!(out, "");
    }

    #[test]
    fn simple_function() {
        let body = block(2, vec![], Some(int_lit(3, "42")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("answer"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("fn answer()"), "got: {out}");
        assert!(out.contains("42"), "got: {out}");
    }

    #[test]
    fn public_function_with_params() {
        let body = block(
            5,
            vec![],
            Some(node(
                6,
                NodeKind::BinaryOp {
                    op: BinOp::Add,
                    left: Box::new(id_node(7, "a")),
                    right: Box::new(id_node(8, "b")),
                },
            )),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("add"),
                generic_params: vec![],
                params: vec![
                    typed_param_node(2, "a", "Int"),
                    typed_param_node(3, "b", "Int"),
                ],
                return_type: Some(Box::new(node(
                    4,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("pub fn add(a: i64, b: i64) -> i64"),
            "got: {out}"
        );
        assert!(out.contains("(a + b)"), "got: {out}");
    }

    #[test]
    fn record_to_struct() {
        let record = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                fields: vec![record_field("x", "Float"), record_field("y", "Float")],
            },
        );
        let out = gen(&module(vec![], vec![record]));
        assert!(out.contains("pub struct Point {"), "got: {out}");
        assert!(out.contains("pub x: f64,"), "got: {out}");
        assert!(out.contains("pub y: f64,"), "got: {out}");
    }

    #[test]
    fn enum_with_variants() {
        let e = node(
            1,
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Color"),
                generic_params: vec![],
                variants: vec![
                    node(
                        2,
                        NodeKind::EnumVariant {
                            name: ident("Red"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                    node(
                        3,
                        NodeKind::EnumVariant {
                            name: ident("Green"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                    node(
                        4,
                        NodeKind::EnumVariant {
                            name: ident("Rgb"),
                            payload: EnumVariantPayload::Struct(vec![
                                record_field("r", "Int"),
                                record_field("g", "Int"),
                            ]),
                        },
                    ),
                    node(
                        7,
                        NodeKind::EnumVariant {
                            name: ident("Custom"),
                            payload: EnumVariantPayload::Tuple(vec![node(
                                8,
                                NodeKind::TypeNamed {
                                    path: type_path(&["String"]),
                                    args: vec![],
                                },
                            )]),
                        },
                    ),
                ],
            },
        );
        let out = gen(&module(vec![], vec![e]));
        assert!(out.contains("pub enum Color {"), "got: {out}");
        assert!(out.contains("Red,"), "got: {out}");
        assert!(out.contains("Green,"), "got: {out}");
        assert!(out.contains("Rgb {"), "got: {out}");
        assert!(out.contains("r: i64,"), "got: {out}");
        assert!(out.contains("Custom(String),"), "got: {out}");
    }

    #[test]
    fn trait_declaration() {
        let t = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Printable"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("print"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(3, vec![], None)),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![t]));
        assert!(out.contains("pub trait Printable {"), "got: {out}");
        assert!(out.contains("fn print(&self);"), "got: {out}");
    }

    #[test]
    fn impl_block() {
        let imp = node(
            1,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: Some(type_path(&["Printable"])),
                trait_args: vec![],
                target: Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["Point"]),
                        args: vec![],
                    },
                )),
                where_clause: vec![],
                methods: vec![node(
                    3,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("print"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], Some(str_lit(5, "point")))),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![imp]));
        assert!(out.contains("impl Printable for Point {"), "got: {out}");
        assert!(out.contains("fn print(&self)"), "got: {out}");
    }

    fn self_param(id: u32) -> AIRNode {
        node(
            id,
            NodeKind::Param {
                pattern: Box::new(bind_pat(id + 100, "self")),
                ty: None,
                default: None,
            },
        )
    }

    /// A method whose declared params lead with `self` must emit a native
    /// `&self` receiver — not both `&self` and a stray `self: _` param
    /// (codegen-correctness defect 3).
    #[test]
    fn self_method_consumes_self_param() {
        let field = node(
            10,
            NodeKind::FieldAccess {
                object: Box::new(id_node(11, "self")),
                field: ident("x"),
            },
        );
        let imp = node(
            1,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["Point"]),
                        args: vec![],
                    },
                )),
                where_clause: vec![],
                methods: vec![node(
                    3,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("get_x"),
                        generic_params: vec![],
                        params: vec![self_param(4)],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(5, vec![], Some(field))),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![imp]));
        assert!(out.contains("fn get_x(&self)"), "got: {out}");
        assert!(
            !out.contains("self: _"),
            "self param leaked as a positional param: {out}"
        );
    }

    /// A desugared instance call `Call(FieldAccess(p, m), [p, x])` emits
    /// `p.m(x)` — the prepended self arg is dropped (defect 3, call site).
    #[test]
    fn self_method_call_drops_prepended_self() {
        let recv = id_node(20, "p");
        let callee = node(
            21,
            NodeKind::FieldAccess {
                object: Box::new(recv.clone()),
                field: ident("scale"),
            },
        );
        let call = node(
            22,
            NodeKind::Call {
                callee: Box::new(callee),
                // First arg shares the receiver\'s NodeId (id 20) — the marker
                // the lowerer sets for a desugared method call.
                args: vec![
                    AirArg {
                        label: None,
                        value: recv,
                    },
                    AirArg {
                        label: None,
                        value: int_lit(23, "4"),
                    },
                ],
                type_args: vec![],
            },
        );
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&call).unwrap();
        assert_eq!(ctx.buf, "p.scale(4_i64)", "got: {}", ctx.buf);
    }

    /// Build a desugared `recv.method(extra)` call carrying the checker's
    /// `recv_kind` annotation, as the primitive-bridge consumer sees it.
    fn annotated_bridge_call(method: &str, tag: &str, extra: Vec<AIRNode>) -> AIRNode {
        let recv = int_lit(20, "1");
        let callee = node(
            21,
            NodeKind::FieldAccess {
                object: Box::new(recv.clone()),
                field: ident(method),
            },
        );
        let mut args = vec![AirArg {
            label: None,
            value: recv,
        }];
        args.extend(extra.into_iter().map(|value| AirArg { label: None, value }));
        let mut call = node(
            22,
            NodeKind::Call {
                callee: Box::new(callee),
                args,
                type_args: vec![],
            },
        );
        call.metadata.insert(
            bock_types::checker::RECV_KIND_META_KEY.to_string(),
            bock_air::Value::String(tag.to_string()),
        );
        call
    }

    /// The Rust backend consumes the `recv_kind` annotation: `(1).compare(2)` on
    /// an `Int` lowers to `i64::cmp` (not the failing `1_i64.compare(2_i64)`).
    #[test]
    fn primitive_bridge_compare_int_emits_cmp() {
        let call = annotated_bridge_call("compare", "Primitive:Int", vec![int_lit(23, "2")]);
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&call).unwrap();
        assert_eq!(ctx.buf, "(1_i64).cmp(&(2_i64))", "got: {}", ctx.buf);
    }

    /// A float `compare` uses `partial_cmp(...).unwrap()` (floats are `PartialOrd`).
    #[test]
    fn primitive_bridge_compare_float_uses_partial_cmp() {
        let recv = node(
            20,
            NodeKind::Literal {
                lit: Literal::Float("1.0".into()),
            },
        );
        let callee = node(
            21,
            NodeKind::FieldAccess {
                object: Box::new(recv.clone()),
                field: ident("compare"),
            },
        );
        let mut call = node(
            22,
            NodeKind::Call {
                callee: Box::new(callee),
                args: vec![
                    AirArg {
                        label: None,
                        value: recv,
                    },
                    AirArg {
                        label: None,
                        value: node(
                            23,
                            NodeKind::Literal {
                                lit: Literal::Float("2.0".into()),
                            },
                        ),
                    },
                ],
                type_args: vec![],
            },
        );
        call.metadata.insert(
            bock_types::checker::RECV_KIND_META_KEY.to_string(),
            bock_air::Value::String("Primitive:Float".to_string()),
        );
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&call).unwrap();
        assert_eq!(
            ctx.buf, "(1.0_f64).partial_cmp(&(2.0_f64)).unwrap()",
            "got: {}",
            ctx.buf
        );
    }

    /// `eq` lowers to `==`; `to_string` to `.to_string()`.
    #[test]
    fn primitive_bridge_eq_and_to_string() {
        let eq_call = annotated_bridge_call("eq", "Primitive:Int", vec![int_lit(23, "2")]);
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&eq_call).unwrap();
        assert_eq!(ctx.buf, "((1_i64) == (2_i64))", "got: {}", ctx.buf);

        let ts_call = annotated_bridge_call("to_string", "Primitive:Int", vec![]);
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&ts_call).unwrap();
        assert_eq!(ctx.buf, "(1_i64).to_string()", "got: {}", ctx.buf);
    }

    /// Without the annotation, the call falls through to the generic
    /// desugared-self-call lowering (no bridge) — so the annotation is what
    /// drives the bridge.
    #[test]
    fn no_annotation_no_bridge() {
        let recv = int_lit(20, "1");
        let callee = node(
            21,
            NodeKind::FieldAccess {
                object: Box::new(recv.clone()),
                field: ident("compare"),
            },
        );
        let call = node(
            22,
            NodeKind::Call {
                callee: Box::new(callee),
                args: vec![
                    AirArg {
                        label: None,
                        value: recv,
                    },
                    AirArg {
                        label: None,
                        value: int_lit(23, "2"),
                    },
                ],
                type_args: vec![],
            },
        );
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&call).unwrap();
        // Generic desugared-self path: `recv.compare(rest)`.
        assert_eq!(ctx.buf, "1_i64.compare(2_i64)", "got: {}", ctx.buf);
    }

    /// Prelude `Ordering` variants lower to Rust's native `std::cmp::Ordering`,
    /// self-contained without the `core.compare` enum decl.
    #[test]
    fn ordering_variant_emits_std_cmp_ordering() {
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&id_node(1, "Less")).unwrap();
        assert_eq!(ctx.buf, "std::cmp::Ordering::Less", "got: {}", ctx.buf);
    }

    #[test]
    fn effect_as_trait() {
        let eff = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Log"),
                generic_params: vec![],
                components: vec![],
                operations: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("log"),
                        generic_params: vec![],
                        params: vec![typed_param_node(3, "msg", "String")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![eff]));
        assert!(out.contains("pub trait Log {"), "got: {out}");
        assert!(out.contains("fn log(&self, msg: String)"), "got: {out}");
    }

    #[test]
    fn function_with_effects() {
        let body = block(3, vec![], Some(int_lit(4, "0")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("process"),
                generic_params: vec![],
                params: vec![typed_param_node(2, "data", "String")],
                return_type: Some(Box::new(node(
                    5,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![type_path(&["Log"]), type_path(&["Clock"])],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("pub fn process(data: String, log: &impl Log, clock: &impl Clock) -> i64"),
            "got: {out}"
        );
    }

    #[test]
    fn ownership_borrow() {
        let borrow = node(
            1,
            NodeKind::Borrow {
                expr: Box::new(id_node(2, "x")),
            },
        );
        let m = module(
            vec![],
            vec![node(
                3,
                NodeKind::FnDecl {
                    annotations: vec![],
                    visibility: Visibility::Private,
                    is_async: false,
                    name: ident("test"),
                    generic_params: vec![],
                    params: vec![],
                    return_type: None,
                    effect_clause: vec![],
                    where_clause: vec![],
                    body: Box::new(block(4, vec![], Some(borrow))),
                },
            )],
        );
        let out = gen(&m);
        assert!(out.contains("&x"), "got: {out}");
    }

    #[test]
    fn ownership_mutable_borrow() {
        let mborrow = node(
            1,
            NodeKind::MutableBorrow {
                expr: Box::new(id_node(2, "x")),
            },
        );
        let m = module(
            vec![],
            vec![node(
                3,
                NodeKind::FnDecl {
                    annotations: vec![],
                    visibility: Visibility::Private,
                    is_async: false,
                    name: ident("test"),
                    generic_params: vec![],
                    params: vec![],
                    return_type: None,
                    effect_clause: vec![],
                    where_clause: vec![],
                    body: Box::new(block(4, vec![], Some(mborrow))),
                },
            )],
        );
        let out = gen(&m);
        assert!(out.contains("&mut x"), "got: {out}");
    }

    #[test]
    fn let_binding_with_mut() {
        let let_node = node(
            1,
            NodeKind::LetBinding {
                is_mut: true,
                pattern: Box::new(bind_pat(2, "x")),
                ty: Some(Box::new(node(
                    3,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                value: Box::new(int_lit(4, "42")),
            },
        );
        let m = module(
            vec![],
            vec![node(
                5,
                NodeKind::FnDecl {
                    annotations: vec![],
                    visibility: Visibility::Private,
                    is_async: false,
                    name: ident("test"),
                    generic_params: vec![],
                    params: vec![],
                    return_type: None,
                    effect_clause: vec![],
                    where_clause: vec![],
                    body: Box::new(block(6, vec![let_node], None)),
                },
            )],
        );
        let out = gen(&m);
        assert!(out.contains("let mut x: i64 = 42_i64;"), "got: {out}");
    }

    #[test]
    fn match_expression() {
        let m_node = node(
            1,
            NodeKind::Match {
                scrutinee: Box::new(id_node(2, "color")),
                arms: vec![
                    node(
                        3,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(
                                4,
                                NodeKind::ConstructorPat {
                                    path: type_path(&["Color", "Red"]),
                                    fields: vec![],
                                },
                            )),
                            guard: None,
                            body: Box::new(block(5, vec![], Some(str_lit(6, "red")))),
                        },
                    ),
                    node(
                        7,
                        NodeKind::MatchArm {
                            pattern: Box::new(node(8, NodeKind::WildcardPat)),
                            guard: None,
                            body: Box::new(block(9, vec![], Some(str_lit(10, "other")))),
                        },
                    ),
                ],
            },
        );
        let f = node(
            11,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(12, vec![m_node], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("match color"), "got: {out}");
        assert!(out.contains("Color::Red =>"), "got: {out}");
        assert!(out.contains("_ =>"), "got: {out}");
    }

    /// Build a `String`-typed param `fn <name>(s: String) -> String { match s { <arms> } }`.
    fn str_match_fn(name: &str, arms: Vec<AIRNode>) -> AIRNode {
        let m = node(
            900,
            NodeKind::Match {
                scrutinee: Box::new(id_node(901, "s")),
                arms,
            },
        );
        node(
            910,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![typed_param_node(911, "s", "String")],
                return_type: Some(Box::new(node(
                    913,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(912, vec![m], None)),
            },
        )
    }

    fn str_lit_pat(id: u32, val: &str) -> AIRNode {
        node(
            id,
            NodeKind::LiteralPat {
                lit: Literal::String(val.into()),
            },
        )
    }

    fn arm(id: u32, pattern: AIRNode, body: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::MatchArm {
                pattern: Box::new(pattern),
                guard: None,
                body: Box::new(body),
            },
        )
    }

    /// A `String` scrutinee matched against `&str` literal arms must match on
    /// `(s).as_str()` so the literal patterns (`"hello"`, type `&str`) line up
    /// with the scrutinee (E0308 otherwise: `String` vs `&str`).
    #[test]
    fn rust_str_literal_match_uses_as_str() {
        let f = str_match_fn(
            "classify_string",
            vec![
                arm(
                    20,
                    str_lit_pat(21, "hello"),
                    block(22, vec![], Some(str_lit(23, "greeting"))),
                ),
                arm(
                    24,
                    str_lit_pat(25, "bye"),
                    block(26, vec![], Some(str_lit(27, "farewell"))),
                ),
                arm(
                    28,
                    node(29, NodeKind::WildcardPat),
                    block(30, vec![], Some(str_lit(31, "unknown"))),
                ),
            ],
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("match (s).as_str()"), "got: {out}");
        // And the whole module must compile (no E0308).
        assert!(
            check_rs_syntax(&out),
            "generated rust did not compile: {out}"
        );
    }

    /// Guard against over-broadening: a `String` scrutinee with no string-literal
    /// arms (here a bare binding arm) must NOT be `.as_str()`-wrapped — that would
    /// rebind the value as `&str` and change its type.
    #[test]
    fn rust_str_literal_match_non_literal_unchanged() {
        let f = str_match_fn(
            "echo_string",
            vec![arm(
                40,
                bind_pat(41, "other"),
                block(42, vec![], Some(id_node(43, "other"))),
            )],
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(!out.contains(".as_str()"), "should not wrap: {out}");
        assert!(out.contains("match s"), "got: {out}");
        assert!(
            check_rs_syntax(&out),
            "generated rust did not compile: {out}"
        );
    }

    /// A `String` match mixing string-literal arms with a top-level *binding* arm
    /// (`other => other`) must NOT be `.as_str()`-wrapped: wrapping would retype
    /// `other` from owned `String` to `&str` and break a body that returns it as a
    /// `String`. The binding arm's presence suppresses the wrap (the literal arm is
    /// left as-is — a separately-tracked residual, not made worse by this fix).
    #[test]
    fn rust_str_literal_match_with_binding_arm_unwrapped() {
        let f = str_match_fn(
            "describe_string",
            vec![
                arm(
                    44,
                    str_lit_pat(45, "hi"),
                    block(46, vec![], Some(str_lit(47, "greeting"))),
                ),
                arm(
                    48,
                    bind_pat(49, "other"),
                    block(70, vec![], Some(id_node(71, "other"))),
                ),
            ],
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains(".as_str()"),
            "binding arm must suppress the wrap: {out}"
        );
        assert!(out.contains("match s"), "got: {out}");
    }

    /// The `.as_str()` wrapping must apply in expression position too (a `match`
    /// used as the value of an enclosing expression), not only statement position.
    #[test]
    fn rust_str_literal_match_expr_position() {
        // fn label(s: String) -> String { let r: String = match s { "y" => "yes", _ => "no" }; r }
        let m = node(
            50,
            NodeKind::Match {
                scrutinee: Box::new(id_node(51, "s")),
                arms: vec![
                    arm(
                        52,
                        str_lit_pat(53, "y"),
                        block(54, vec![], Some(str_lit(55, "yes"))),
                    ),
                    arm(
                        56,
                        node(57, NodeKind::WildcardPat),
                        block(58, vec![], Some(str_lit(59, "no"))),
                    ),
                ],
            },
        );
        let let_node = node(
            60,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(61, "r")),
                ty: Some(Box::new(node(
                    62,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                value: Box::new(m),
            },
        );
        let f = node(
            63,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("label"),
                generic_params: vec![],
                params: vec![typed_param_node(64, "s", "String")],
                return_type: Some(Box::new(node(
                    67,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(65, vec![let_node], Some(id_node(66, "r")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("match (s).as_str()"), "got: {out}");
        assert!(
            check_rs_syntax(&out),
            "generated rust did not compile: {out}"
        );
    }

    #[test]
    fn string_interpolation() {
        let interp = node(
            1,
            NodeKind::Interpolation {
                parts: vec![
                    AirInterpolationPart::Literal("Hello, ".into()),
                    AirInterpolationPart::Expr(Box::new(id_node(2, "name"))),
                    AirInterpolationPart::Literal("!".into()),
                ],
            },
        );
        let f = node(
            3,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![], Some(interp))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("format!(\"Hello, {}!\", name)"), "got: {out}");
    }

    #[test]
    fn result_construct() {
        let ok = node(
            1,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Ok,
                value: Some(Box::new(int_lit(2, "42"))),
            },
        );
        let err = node(
            3,
            NodeKind::ResultConstruct {
                variant: ResultVariant::Err,
                value: Some(Box::new(str_lit(4, "oops"))),
            },
        );
        let f = node(
            5,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(6, vec![], Some(ok))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("Ok(42_i64)"), "got: {out}");

        let f2 = node(
            7,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test2"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(8, vec![], Some(err))),
            },
        );
        let out2 = gen(&module(vec![], vec![f2]));
        assert!(out2.contains("Err(\"oops\".to_string())"), "got: {out2}");
    }

    #[test]
    fn vec_literal() {
        let list = node(
            1,
            NodeKind::ListLiteral {
                elems: vec![int_lit(2, "1"), int_lit(3, "2"), int_lit(4, "3")],
            },
        );
        let f = node(
            5,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(6, vec![], Some(list))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("vec![1_i64, 2_i64, 3_i64]"), "got: {out}");
    }

    #[test]
    fn propagate_operator() {
        let prop = node(
            1,
            NodeKind::Propagate {
                expr: Box::new(node(
                    2,
                    NodeKind::Call {
                        callee: Box::new(id_node(3, "parse")),
                        args: vec![],
                        type_args: vec![],
                    },
                )),
            },
        );
        let f = node(
            4,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], Some(prop))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("parse()?"), "got: {out}");
    }

    #[test]
    fn range_expression() {
        let range = node(
            1,
            NodeKind::Range {
                lo: Box::new(int_lit(2, "0")),
                hi: Box::new(int_lit(3, "10")),
                inclusive: false,
            },
        );
        let range_incl = node(
            4,
            NodeKind::Range {
                lo: Box::new(int_lit(5, "0")),
                hi: Box::new(int_lit(6, "10")),
                inclusive: true,
            },
        );
        let f = node(
            7,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(8, vec![], Some(range))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("0_i64..10_i64"), "got: {out}");

        let f2 = node(
            9,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test2"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(10, vec![], Some(range_incl))),
            },
        );
        let out2 = gen(&module(vec![], vec![f2]));
        assert!(out2.contains("0_i64..=10_i64"), "got: {out2}");
    }

    #[test]
    fn generics_with_bounds() {
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("show"),
                generic_params: vec![GenericParam {
                    id: 100,
                    span: span(),
                    name: ident("T"),
                    bounds: vec![type_path(&["Display"])],
                }],
                params: vec![typed_param_node(2, "val", "T")],
                return_type: Some(Box::new(node(
                    3,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(4, vec![], Some(id_node(5, "val")))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("pub fn show<T: Display>(val: T) -> String"),
            "got: {out}"
        );
    }

    #[test]
    fn type_alias() {
        let alias = node(
            1,
            NodeKind::TypeAlias {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Coord"),
                generic_params: vec![],
                ty: Box::new(node(
                    2,
                    NodeKind::TypeTuple {
                        elems: vec![
                            node(
                                3,
                                NodeKind::TypeNamed {
                                    path: type_path(&["Float"]),
                                    args: vec![],
                                },
                            ),
                            node(
                                4,
                                NodeKind::TypeNamed {
                                    path: type_path(&["Float"]),
                                    args: vec![],
                                },
                            ),
                        ],
                    },
                )),
                where_clause: vec![],
            },
        );
        let out = gen(&module(vec![], vec![alias]));
        assert!(out.contains("pub type Coord = (f64, f64);"), "got: {out}");
    }

    #[test]
    fn const_declaration() {
        let c = node(
            1,
            NodeKind::ConstDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("MaxSize"),
                ty: Box::new(node(
                    2,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                )),
                value: Box::new(int_lit(3, "100")),
            },
        );
        let out = gen(&module(vec![], vec![c]));
        assert!(
            out.contains("pub const MAX_SIZE: i64 = 100_i64;"),
            "got: {out}"
        );
    }

    #[test]
    fn import_declaration_is_dropped() {
        // In the single-module self-contained emit (`generate_module`), there is
        // no sibling module to import from, so a Bock `ImportDecl` emits nothing.
        // (The per-module project path emits real `use crate::<m>::<x>;`.)
        let imp = node(
            1,
            NodeKind::ImportDecl {
                path: mod_path(&["core", "compare"]),
                items: ImportItems::Named(vec![imported_name("Key"), imported_name("key")]),
            },
        );
        let out = gen(&module(vec![imp], vec![]));
        assert!(
            !out.contains("use core::compare"),
            "ImportDecl must be a no-op in single-module emit; got: {out}"
        );
    }

    #[test]
    fn for_loop() {
        let body = block(
            3,
            vec![node(
                4,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(5, "y")),
                    ty: None,
                    value: Box::new(id_node(6, "x")),
                },
            )],
            None,
        );
        let for_node = node(
            1,
            NodeKind::For {
                pattern: Box::new(bind_pat(2, "x")),
                iterable: Box::new(id_node(7, "items")),
                body: Box::new(body),
            },
        );
        let f = node(
            8,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(9, vec![for_node], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("for x in items {"), "got: {out}");
        assert!(out.contains("let y = x;"), "got: {out}");
    }

    #[test]
    fn await_expression() {
        let aw = node(
            1,
            NodeKind::Await {
                expr: Box::new(node(
                    2,
                    NodeKind::Call {
                        callee: Box::new(id_node(3, "fetch")),
                        args: vec![],
                        type_args: vec![],
                    },
                )),
            },
        );
        let f = node(
            4,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], Some(aw))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("async fn test()"), "got: {out}");
        assert!(out.contains("fetch().await"), "got: {out}");
    }

    #[test]
    fn async_main_gets_tokio_main_attribute() {
        let body = block(2, vec![], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("#[tokio::main]"), "got: {out}");
        assert!(out.contains("async fn main()"), "got: {out}");
    }

    #[test]
    fn sync_main_no_tokio_attribute() {
        let body = block(2, vec![], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(!out.contains("#[tokio::main]"), "got: {out}");
        assert!(out.contains("fn main()"), "got: {out}");
    }

    #[test]
    fn concurrent_pattern_spawns_tasks() {
        // Two async calls bound to locals, then awaited later in same block —
        // should wrap each in `tokio::spawn` and unwrap JoinHandles on await.
        let call_fetch = |id: u32, name: &str| {
            node(
                id,
                NodeKind::Call {
                    callee: Box::new(id_node(id + 1, name)),
                    args: vec![],
                    type_args: vec![],
                },
            )
        };
        let let_stmt = |id: u32, name: &str, val: AIRNode| {
            node(
                id,
                NodeKind::LetBinding {
                    is_mut: false,
                    pattern: Box::new(bind_pat(id + 1, name)),
                    ty: None,
                    value: Box::new(val),
                },
            )
        };
        let await_id = |id: u32, name: &str| {
            node(
                id,
                NodeKind::Await {
                    expr: Box::new(id_node(id + 1, name)),
                },
            )
        };
        let body = block(
            10,
            vec![
                let_stmt(20, "a", call_fetch(21, "task1")),
                let_stmt(30, "b", call_fetch(31, "task2")),
                let_stmt(40, "ra", await_id(41, "a")),
                let_stmt(50, "rb", await_id(51, "b")),
            ],
            Some(id_node(60, "ra")),
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("run"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("let a = tokio::spawn(task1());"),
            "task1 should be spawned, got: {out}"
        );
        assert!(
            out.contains("let b = tokio::spawn(task2());"),
            "task2 should be spawned, got: {out}"
        );
        assert!(
            out.contains("let ra = a.await.unwrap();"),
            "join handle `a` should be unwrapped on await, got: {out}"
        );
        assert!(
            out.contains("let rb = b.await.unwrap();"),
            "join handle `b` should be unwrapped on await, got: {out}"
        );
    }

    #[test]
    fn sequential_await_no_spawn() {
        // `let a = await task1()` directly awaits — no spawn wrap.
        let await_call = node(
            20,
            NodeKind::Await {
                expr: Box::new(node(
                    21,
                    NodeKind::Call {
                        callee: Box::new(id_node(22, "task1")),
                        args: vec![],
                        type_args: vec![],
                    },
                )),
            },
        );
        let let_stmt = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "a")),
                ty: None,
                value: Box::new(await_call),
            },
        );
        let body = block(30, vec![let_stmt], Some(id_node(40, "a")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: true,
                name: ident("run"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            !out.contains("tokio::spawn"),
            "sequential await should not spawn, got: {out}"
        );
        assert!(out.contains("let a = task1().await;"), "got: {out}");
    }

    #[test]
    fn record_construct() {
        let rc = node(
            1,
            NodeKind::RecordConstruct {
                path: type_path(&["Point"]),
                fields: vec![
                    AirRecordField {
                        name: ident("x"),
                        value: Some(Box::new(int_lit(2, "1"))),
                    },
                    AirRecordField {
                        name: ident("y"),
                        value: Some(Box::new(int_lit(3, "2"))),
                    },
                ],
                spread: None,
            },
        );
        let f = node(
            4,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], Some(rc))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("Point { x: 1_i64, y: 2_i64 }"), "got: {out}");
    }

    #[test]
    fn map_literal() {
        let map = node(
            1,
            NodeKind::MapLiteral {
                entries: vec![AirMapEntry {
                    key: str_lit(2, "key"),
                    value: int_lit(3, "42"),
                }],
            },
        );
        let f = node(
            4,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![], Some(map))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("std::collections::HashMap::from([(\"key\".to_string(), 42_i64)])"),
            "got: {out}"
        );
    }

    #[test]
    fn tuple_literal() {
        let tup = node(
            1,
            NodeKind::TupleLiteral {
                elems: vec![int_lit(2, "1"), str_lit(3, "hello"), bool_lit(4, true)],
            },
        );
        let f = node(
            5,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(6, vec![], Some(tup))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("(1_i64, \"hello\".to_string(), true)"),
            "got: {out}"
        );
    }

    #[test]
    fn unreachable_expression() {
        let unr = node(1, NodeKind::Unreachable);
        let f = node(
            2,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("test"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(3, vec![], Some(unr))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("unreachable!()"), "got: {out}");
    }

    #[test]
    fn escape_strings() {
        assert_eq!(escape_rs_string("hello"), "hello");
        assert_eq!(escape_rs_string("he\"llo"), "he\\\"llo");
        assert_eq!(escape_rs_string("new\nline"), "new\\nline");
    }

    #[test]
    fn escape_format_strings() {
        assert_eq!(escape_format_string("hello"), "hello");
        assert_eq!(escape_format_string("{test}"), "{{test}}");
    }

    #[test]
    fn to_snake_case_conversions() {
        assert_eq!(to_snake_case("hello"), "hello");
        assert_eq!(to_snake_case("HelloWorld"), "hello_world");
        assert_eq!(to_snake_case("camelCase"), "camel_case");
        assert_eq!(to_snake_case("HTTPClient"), "http_client");
        assert_eq!(to_snake_case("_"), "_");
    }

    #[test]
    fn to_upper_snake_case_conversions() {
        assert_eq!(to_upper_snake_case("MaxSize"), "MAX_SIZE");
        assert_eq!(to_upper_snake_case("httpClient"), "HTTP_CLIENT");
    }

    // ── End-to-end syntax validation tests ──────────────────────────────────

    #[test]
    #[ignore]
    fn e2e_simple_function_compiles() {
        let body = block(2, vec![], Some(int_lit(3, "42")));
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("answer"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    4,
                    NodeKind::TypeNamed {
                        path: type_path(&["Int"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            check_rs_syntax(&out),
            "Generated Rust does not compile:\n{out}"
        );
    }

    #[test]
    #[ignore]
    fn e2e_struct_compiles() {
        let record = node(
            1,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Point"),
                generic_params: vec![],
                fields: vec![record_field("x", "Float"), record_field("y", "Float")],
            },
        );
        let out = gen(&module(vec![], vec![record]));
        assert!(
            check_rs_syntax(&out),
            "Generated Rust does not compile:\n{out}"
        );
    }

    #[test]
    #[ignore]
    fn e2e_enum_compiles() {
        let e = node(
            1,
            NodeKind::EnumDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Color"),
                generic_params: vec![],
                variants: vec![
                    node(
                        2,
                        NodeKind::EnumVariant {
                            name: ident("Red"),
                            payload: EnumVariantPayload::Unit,
                        },
                    ),
                    node(
                        3,
                        NodeKind::EnumVariant {
                            name: ident("Rgb"),
                            payload: EnumVariantPayload::Struct(vec![record_field("r", "Int")]),
                        },
                    ),
                    node(
                        5,
                        NodeKind::EnumVariant {
                            name: ident("Custom"),
                            payload: EnumVariantPayload::Tuple(vec![node(
                                6,
                                NodeKind::TypeNamed {
                                    path: type_path(&["String"]),
                                    args: vec![],
                                },
                            )]),
                        },
                    ),
                ],
            },
        );
        let out = gen(&module(vec![], vec![e]));
        assert!(
            check_rs_syntax(&out),
            "Generated Rust does not compile:\n{out}"
        );
    }

    #[test]
    #[ignore]
    fn e2e_trait_and_impl_compiles() {
        let trait_decl = node(
            1,
            NodeKind::TraitDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_platform: false,
                name: ident("Greet"),
                generic_params: vec![],
                associated_types: vec![],
                methods: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("greet"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: Some(Box::new(node(
                            3,
                            NodeKind::TypeNamed {
                                path: type_path(&["String"]),
                                args: vec![],
                            },
                        ))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );
        let struct_decl = node(
            10,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Person"),
                generic_params: vec![],
                fields: vec![record_field("name", "String")],
            },
        );
        let impl_block = node(
            20,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: Some(type_path(&["Greet"])),
                trait_args: vec![],
                target: Box::new(node(
                    21,
                    NodeKind::TypeNamed {
                        path: type_path(&["Person"]),
                        args: vec![],
                    },
                )),
                where_clause: vec![],
                methods: vec![node(
                    22,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("greet"),
                        generic_params: vec![],
                        params: vec![],
                        return_type: Some(Box::new(node(
                            23,
                            NodeKind::TypeNamed {
                                path: type_path(&["String"]),
                                args: vec![],
                            },
                        ))),
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(24, vec![], Some(str_lit(25, "hello")))),
                    },
                )],
            },
        );
        let out = gen(&module(vec![], vec![trait_decl, struct_decl, impl_block]));
        assert!(
            check_rs_syntax(&out),
            "Generated Rust does not compile:\n{out}"
        );
    }

    // ── Prelude function mapping tests ──────────────────────────────────────

    /// Helper: generate Rust for a module with a `main` function containing a single call.
    fn gen_prelude_call(func_name: &str, arg: AIRNode) -> String {
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, func_name)),
                args: vec![AirArg {
                    label: None,
                    value: arg,
                }],
                type_args: vec![],
            },
        );
        let body = block(2, vec![call], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                name: ident("main"),
                params: vec![],
                return_type: None,
                body: Box::new(body),
                generic_params: vec![],
                visibility: Visibility::Private,
                annotations: vec![],
                effect_clause: vec![],
                where_clause: vec![],
                is_async: false,
            },
        );
        gen(&module(vec![], vec![f]))
    }

    /// Helper: generate Rust for a nullary prelude call (no args).
    fn gen_prelude_call_no_args(func_name: &str) -> String {
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, func_name)),
                args: vec![],
                type_args: vec![],
            },
        );
        let body = block(2, vec![call], None);
        let f = node(
            1,
            NodeKind::FnDecl {
                name: ident("main"),
                params: vec![],
                return_type: None,
                body: Box::new(body),
                generic_params: vec![],
                visibility: Visibility::Private,
                annotations: vec![],
                effect_clause: vec![],
                where_clause: vec![],
                is_async: false,
            },
        );
        gen(&module(vec![], vec![f]))
    }

    #[test]
    fn prelude_println_maps_to_println_macro() {
        let out = gen_prelude_call("println", str_lit(12, "hello"));
        assert!(
            out.contains("println!(\"{}\", "),
            "println should map to println! macro, got: {out}"
        );
        assert!(
            !out.contains("println("),
            "should not emit bare println(, got: {out}"
        );
    }

    #[test]
    fn prelude_print_maps_to_print_macro() {
        let out = gen_prelude_call("print", str_lit(12, "hello"));
        assert!(
            out.contains("print!(\"{}\", "),
            "print should map to print! macro, got: {out}"
        );
    }

    #[test]
    fn prelude_debug_maps_to_dbg_macro() {
        let out = gen_prelude_call("debug", str_lit(12, "val"));
        assert!(
            out.contains("dbg!(&"),
            "debug should map to dbg! macro, got: {out}"
        );
    }

    #[test]
    fn prelude_assert_maps_to_assert_macro() {
        let out = gen_prelude_call("assert", bool_lit(12, true));
        assert!(
            out.contains("assert!("),
            "assert should map to assert! macro, got: {out}"
        );
    }

    #[test]
    fn prelude_todo_maps_to_todo_macro() {
        let out = gen_prelude_call_no_args("todo");
        assert!(
            out.contains("todo!()"),
            "todo should map to todo! macro, got: {out}"
        );
    }

    #[test]
    fn prelude_unreachable_maps_to_unreachable_macro() {
        let out = gen_prelude_call_no_args("unreachable");
        assert!(
            out.contains("unreachable!()"),
            "unreachable should map to unreachable! macro, got: {out}"
        );
    }

    #[test]
    fn non_prelude_call_passes_through() {
        let out = gen_prelude_call("my_custom_func", str_lit(12, "arg"));
        assert!(
            out.contains("my_custom_func("),
            "non-prelude call should use snake_case, got: {out}"
        );
    }

    #[test]
    fn handling_block_passes_handlers_to_effectful_call() {
        use bock_air::AirHandlerPair;

        // effect Logger { fn log(msg: String) -> Void }
        let effect_decl = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("log"),
                        generic_params: vec![],
                        params: vec![typed_param_node(3, "msg", "String")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );

        // fn inner() -> String with Logger { ... }
        let inner_fn = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("inner"),
                generic_params: vec![],
                params: vec![],
                return_type: Some(Box::new(node(
                    11,
                    NodeKind::TypeNamed {
                        path: type_path(&["String"]),
                        args: vec![],
                    },
                ))),
                effect_clause: vec![type_path(&["Logger"])],
                where_clause: vec![],
                body: Box::new(block(12, vec![], Some(str_lit(13, "hello")))),
            },
        );

        // fn main() { handling (Logger with StdoutLogger {}) { inner() } }
        let call_inner = node(
            20,
            NodeKind::Call {
                callee: Box::new(id_node(21, "inner")),
                args: vec![],
                type_args: vec![],
            },
        );
        let handling = node(
            30,
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path(&["Logger"]),
                    handler: Box::new(node(
                        31,
                        NodeKind::Call {
                            callee: Box::new(id_node(32, "StdoutLogger")),
                            args: vec![],
                            type_args: vec![],
                        },
                    )),
                }],
                body: Box::new(block(33, vec![], Some(call_inner))),
            },
        );
        let main_fn = node(
            40,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(41, vec![handling], None)),
            },
        );

        let out = gen(&module(vec![], vec![effect_decl, inner_fn, main_fn]));
        // inner() should receive the handler: inner(&__logger)
        assert!(
            out.contains("inner(&__logger)"),
            "handling block should pass handler to effectful call, got: {out}"
        );
        // The handling block should instantiate the handler. The PascalCase
        // identifier is preserved, since it names a type/constructor in Rust.
        assert!(
            out.contains("let __logger = StdoutLogger()"),
            "handling block should instantiate handler, got: {out}"
        );
    }

    #[test]
    fn nested_handling_blocks_shadow_handlers() {
        use bock_air::AirHandlerPair;

        // effect Logger { fn log(msg: String) -> Void }
        let effect_decl = node(
            1,
            NodeKind::EffectDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                name: ident("Logger"),
                generic_params: vec![],
                components: vec![],
                operations: vec![node(
                    2,
                    NodeKind::FnDecl {
                        annotations: vec![],
                        visibility: Visibility::Public,
                        is_async: false,
                        name: ident("log"),
                        generic_params: vec![],
                        params: vec![typed_param_node(3, "msg", "String")],
                        return_type: None,
                        effect_clause: vec![],
                        where_clause: vec![],
                        body: Box::new(block(4, vec![], None)),
                    },
                )],
            },
        );

        // fn inner() -> String with Logger { ... }
        let inner_fn = node(
            10,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("inner"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![type_path(&["Logger"])],
                where_clause: vec![],
                body: Box::new(block(12, vec![], Some(str_lit(13, "hello")))),
            },
        );

        // Nested handling: inner handling block shadows outer
        let inner_call = node(
            20,
            NodeKind::Call {
                callee: Box::new(id_node(21, "inner")),
                args: vec![],
                type_args: vec![],
            },
        );
        let inner_handling = node(
            30,
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path(&["Logger"]),
                    handler: Box::new(id_node(31, "inner_logger")),
                }],
                body: Box::new(block(32, vec![], Some(inner_call))),
            },
        );
        let outer_handling = node(
            40,
            NodeKind::HandlingBlock {
                handlers: vec![AirHandlerPair {
                    effect: type_path(&["Logger"]),
                    handler: Box::new(id_node(41, "outer_logger")),
                }],
                body: Box::new(block(42, vec![inner_handling], None)),
            },
        );
        let main_fn = node(
            50,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("main"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(51, vec![outer_handling], None)),
            },
        );

        let out = gen(&module(vec![], vec![effect_decl, inner_fn, main_fn]));
        // Inner handling should shadow: inner(&__logger) where __logger = inner_logger
        assert!(
            out.contains("let __logger = inner_logger"),
            "inner handling should shadow outer handler, got: {out}"
        );
        assert!(
            out.contains("inner(&__logger)"),
            "call should use innermost handler, got: {out}"
        );
    }

    // ── Generic impl synthesis (DV12 / P1-b2) ─────────────────────────────────

    fn generic_param(id: u32, name: &str) -> GenericParam {
        GenericParam {
            id,
            span: span(),
            name: ident(name),
            bounds: vec![],
        }
    }

    fn named_type(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::TypeNamed {
                path: type_path(&[name]),
                args: vec![],
            },
        )
    }

    /// `record Box[T] { value: T }`.
    fn generic_box_record() -> AIRNode {
        node(
            10,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                name: ident("Box"),
                generic_params: vec![generic_param(11, "T")],
                fields: vec![RecordDeclField {
                    id: 12,
                    span: span(),
                    name: ident("value"),
                    ty: TypeExpr::Named {
                        id: 13,
                        span: span(),
                        path: type_path(&["T"]),
                        args: vec![],
                    },
                    default: None,
                }],
            },
        )
    }

    /// `impl Box { fn get(self) -> T { return self.value } }` — a getter that
    /// returns a `self` field by value.
    fn generic_box_getter_impl() -> AIRNode {
        let self_param = node(
            20,
            NodeKind::Param {
                pattern: Box::new(bind_pat(21, "self")),
                ty: None,
                default: None,
            },
        );
        let body = block(
            22,
            vec![],
            Some(node(
                23,
                NodeKind::Return {
                    value: Some(Box::new(node(
                        24,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(25, "self")),
                            field: ident("value"),
                        },
                    ))),
                },
            )),
        );
        let method = node(
            26,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("get"),
                generic_params: vec![],
                params: vec![self_param],
                return_type: Some(Box::new(named_type(27, "T"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        node(
            30,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(named_type(31, "Box")),
                where_clause: vec![],
                methods: vec![method],
            },
        )
    }

    #[test]
    fn generic_impl_synthesizes_impl_and_clone_for_getter() {
        // `impl Box { fn get(self) -> T { return self.value } }` for
        // `record Box[T]` must synthesize `impl<T: Clone> Box<T>`, derive
        // `Clone`, and clone the field read (a `&self` method cannot move a
        // non-`Copy` field out).
        let out = gen(&module(
            vec![],
            vec![generic_box_record(), generic_box_getter_impl()],
        ));
        assert!(
            out.contains("#[derive(Clone)]"),
            "generic getter target should derive Clone, got: {out}"
        );
        assert!(
            out.contains("impl<T: Clone> Box<T> {"),
            "impl should synthesize `<T: Clone>` and apply `Box<T>`, got: {out}"
        );
        assert!(
            out.contains("return self.value.clone();"),
            "field return should be cloned, got: {out}"
        );
    }

    #[test]
    fn generic_impl_no_clone_bound_when_field_not_returned() {
        // A generic impl whose method does NOT return a `self` field by value
        // must NOT be over-constrained with a `T: Clone` *impl bound*. (The
        // struct itself now always `#[derive(Clone)]`s — GAP-B — but the derive's
        // own per-field bound is independent of and does not over-constrain the
        // inherent impl.)
        let self_param = node(
            40,
            NodeKind::Param {
                pattern: Box::new(bind_pat(41, "self")),
                ty: None,
                default: None,
            },
        );
        // `fn id_value(self) -> Int { return 0 }` — returns a literal, not a
        // `self` field.
        let body = block(
            42,
            vec![],
            Some(node(
                43,
                NodeKind::Return {
                    value: Some(Box::new(int_lit(44, "0"))),
                },
            )),
        );
        let method = node(
            45,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("zero"),
                generic_params: vec![],
                params: vec![self_param],
                return_type: Some(Box::new(named_type(46, "Int"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let impl_block = node(
            47,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(named_type(48, "Box")),
                where_clause: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![generic_box_record(), impl_block]));
        assert!(
            out.contains("impl<T> Box<T> {"),
            "impl should synthesize `<T>` (no Clone) for a non-returning method, got: {out}"
        );
        assert!(
            !out.contains("T: Clone"),
            "must NOT over-constrain the impl with a `T: Clone` bound, got: {out}"
        );
    }

    #[test]
    fn generic_trait_impl_clones_field_wrapped_in_constructor() {
        // GAP-B: a generic *trait* impl whose method returns `Some(self.value)`
        // moves the field out of `&self`; the body must clone it and the impl
        // must carry a `T: Clone` bound — even though the field is wrapped in a
        // `Some(...)` constructor (not a bare `return self.value`).
        let self_param = node(
            60,
            NodeKind::Param {
                pattern: Box::new(bind_pat(61, "self")),
                ty: None,
                default: None,
            },
        );
        // `return Some(self.value)`.
        let some_call = node(
            62,
            NodeKind::Call {
                callee: Box::new(id_node(63, "Some")),
                args: vec![AirArg {
                    label: None,
                    value: node(
                        64,
                        NodeKind::FieldAccess {
                            object: Box::new(id_node(65, "self")),
                            field: ident("value"),
                        },
                    ),
                }],
                type_args: vec![],
            },
        );
        let body = block(
            66,
            vec![],
            Some(node(
                67,
                NodeKind::Return {
                    value: Some(Box::new(some_call)),
                },
            )),
        );
        let method = node(
            68,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("f"),
                generic_params: vec![],
                params: vec![self_param],
                // `-> Optional[T]`.
                return_type: Some(Box::new(node(
                    69,
                    NodeKind::TypeNamed {
                        path: type_path(&["Optional"]),
                        args: vec![named_type(70, "T")],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let impl_block = node(
            71,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: Some(type_path(&["P"])),
                trait_args: vec![named_type(72, "T")],
                target: Box::new(named_type(73, "Box")),
                where_clause: vec![],
                methods: vec![method],
            },
        );
        let out = gen(&module(vec![], vec![generic_box_record(), impl_block]));
        assert!(
            out.contains("impl<T: Clone> P<T> for Box<T>"),
            "trait impl should synthesize `<T: Clone>` and carry trait args, got: {out}"
        );
        assert!(
            out.contains("self.value.clone()"),
            "field wrapped in Some(...) should still be cloned, got: {out}"
        );
    }

    #[test]
    fn generic_fn_clones_collection_element_gets_bound() {
        // GAP-B (free fn): a generic `fn dup[T](xs: List[T]) -> List[T]` whose
        // body lowers a `concat`/`get` with `.cloned()`/`.clone()` needs a
        // `T: Clone` bound. We model the `concat` desugar shape (a
        // `Call(FieldAccess(xs, "concat"), [xs, xs])`) the checker produces.
        let xs_param = typed_param_node(80, "xs", "List");
        let recv = id_node(82, "xs");
        let concat_call = node(
            83,
            NodeKind::Call {
                callee: Box::new(node(
                    84,
                    NodeKind::FieldAccess {
                        object: Box::new(recv),
                        field: ident("concat"),
                    },
                )),
                args: vec![
                    AirArg {
                        label: None,
                        value: id_node(82, "xs"),
                    },
                    AirArg {
                        label: None,
                        value: id_node(82, "xs"),
                    },
                ],
                type_args: vec![],
            },
        );
        let body = block(
            85,
            vec![],
            Some(node(
                86,
                NodeKind::Return {
                    value: Some(Box::new(concat_call)),
                },
            )),
        );
        let f = node(
            87,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("dup"),
                generic_params: vec![generic_param(88, "T")],
                params: vec![xs_param],
                return_type: Some(Box::new(node(
                    89,
                    NodeKind::TypeNamed {
                        path: type_path(&["List"]),
                        args: vec![named_type(90, "T")],
                    },
                ))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("fn dup<T: Clone>"),
            "generic fn cloning a collection element should get `T: Clone`, got: {out}"
        );
    }

    #[test]
    fn generic_fn_no_clone_bound_without_collection_clone() {
        // A generic fn that does NOT clone a collection element must not be
        // over-constrained with `Clone`.
        let xs_param = typed_param_node(91, "x", "Int");
        let body = block(
            92,
            vec![],
            Some(node(
                93,
                NodeKind::Return {
                    value: Some(Box::new(id_node(94, "x"))),
                },
            )),
        );
        let f = node(
            95,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("identity"),
                generic_params: vec![generic_param(96, "T")],
                params: vec![xs_param],
                return_type: Some(Box::new(named_type(97, "T"))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("fn identity<T>"),
            "non-cloning generic fn should keep a bare `<T>`, got: {out}"
        );
        assert!(
            !out.contains("T: Clone"),
            "must NOT over-constrain a non-cloning generic fn with Clone, got: {out}"
        );
    }

    #[test]
    fn collect_pattern_binding_names_walks_constructor_pat() {
        // `Some(x)` binds `x`; the names are collected for the move-reuse scan.
        let pat = node(
            1,
            NodeKind::ConstructorPat {
                path: type_path(&["Some"]),
                fields: vec![bind_pat(2, "x")],
            },
        );
        let mut names = Vec::new();
        RsEmitCtx::collect_pattern_binding_names(&pat, &mut names);
        assert_eq!(names, vec!["x".to_string()]);
    }

    #[test]
    fn count_identifier_uses_counts_every_read() {
        // A body that reads `x` twice (`pred(x)` then `[x]`) reports 2 uses, so
        // the move-reuse analysis flags `x` as needing a clone-on-second-use.
        let body = block(
            10,
            vec![
                node(
                    11,
                    NodeKind::Call {
                        callee: Box::new(id_node(12, "pred")),
                        args: vec![AirArg {
                            label: None,
                            value: id_node(13, "x"),
                        }],
                        type_args: vec![],
                    },
                ),
                node(
                    14,
                    NodeKind::ListLiteral {
                        elems: vec![id_node(15, "x")],
                    },
                ),
            ],
            None,
        );
        assert_eq!(RsEmitCtx::count_identifier_uses(&body, "x"), 2);
        assert_eq!(RsEmitCtx::count_identifier_uses(&body, "y"), 0);
    }

    // ── Per-module native-module tree (S3) ──────────────────────────────────

    /// A module node with a declared dotted `path` (e.g. `core.option`), used by
    /// the per-module emission tests where the file layout and `mod`/`use`
    /// wiring are keyed on the declared module-path.
    fn module_with_path(path: &[&str], imports: Vec<AIRNode>, items: Vec<AIRNode>) -> AIRNode {
        node(
            0,
            NodeKind::Module {
                path: Some(mod_path(path)),
                annotations: vec![],
                imports,
                items,
            },
        )
    }

    /// An `import <path>.{ name }` AIR node (a single-item `Named` import).
    fn import_named(id: u32, path: &[&str], name: &str) -> AIRNode {
        node(
            id,
            NodeKind::ImportDecl {
                path: mod_path(path),
                items: ImportItems::Named(vec![imported_name(name)]),
            },
        )
    }

    /// A bare `fn <name>() -> <tail>` declaration with the given visibility.
    fn fn_decl_tail(id: u32, vis: Visibility, name: &str, tail: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: vis,
                is_async: false,
                name: ident(name),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(id + 1, vec![], Some(tail))),
            },
        )
    }

    #[test]
    fn per_module_emits_native_rust_module_tree() {
        // entry `module main` uses `mathutil.add_one`; `module mathutil` exports
        // a `public fn add_one`. Per-module emission must produce the native
        // module *source* tree: `src/main.rs` (with `mod mathutil;` + `use
        // crate::mathutil::{add_one};`), and `src/mathutil.rs` — a real module
        // tree, not a single collapsed file. The `Cargo.toml` run affordance is
        // emitted by the scaffolder (project mode), NOT by codegen (S6a / DV18).
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "add_one")),
                args: vec![AirArg {
                    label: None,
                    value: int_lit(12, "6"),
                }],
                type_args: vec![],
            },
        );
        let main_mod = module_with_path(
            &["main"],
            vec![import_named(5, &["mathutil"], "add_one")],
            vec![fn_decl_tail(1, Visibility::Private, "main", call)],
        );
        let util_mod = module_with_path(
            &["mathutil"],
            vec![],
            vec![fn_decl_tail(
                20,
                Visibility::Public,
                "add_one",
                int_lit(22, "7"),
            )],
        );

        let gen = RsGenerator::new();
        let out = gen
            .generate_project(&[
                (&main_mod, std::path::Path::new("src/main.bock")),
                (&util_mod, std::path::Path::new("src/mathutil.bock")),
            ])
            .unwrap();

        let by_name = |p: &str| out.files.iter().find(|f| f.path == std::path::Path::new(p));
        let main_file = by_name("src/main.rs").expect("src/main.rs emitted");
        let util_file = by_name("src/mathutil.rs").expect("src/mathutil.rs emitted");
        // Codegen no longer emits the manifest (S6a / DV18) — the scaffolder
        // owns the `Cargo.toml` in project mode.
        assert!(
            by_name("Cargo.toml").is_none(),
            "codegen must NOT emit Cargo.toml — the scaffolder owns it (S6a)"
        );

        assert!(
            main_file.content.contains("mod mathutil;"),
            "main.rs must declare the sibling module; got:\n{}",
            main_file.content
        );
        assert!(
            main_file
                .content
                .contains("use crate::mathutil::{add_one};"),
            "main.rs must `use` the cross-module fn; got:\n{}",
            main_file.content
        );
        // The inner attribute must precede the `mod` declarations.
        let attr = main_file.content.find("#![allow").expect("inner attr");
        let modline = main_file.content.find("mod mathutil;").unwrap();
        assert!(attr < modline, "inner attribute must precede `mod`");
        assert!(
            util_file.content.contains("pub fn add_one("),
            "mathutil.rs must carry the exported fn; got:\n{}",
            util_file.content
        );
    }

    #[test]
    fn per_module_builds_nested_mod_tree_wiring() {
        // entry uses `core.option.get_or`. The nested `core.option` module must
        // produce `src/core/option.rs` (the leaf), a `src/core.rs` wiring file
        // declaring `pub mod option;`, and `mod core;` at the crate root.
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "get_or")),
                args: vec![],
                type_args: vec![],
            },
        );
        let main_mod = module_with_path(
            &["main"],
            vec![import_named(5, &["core", "option"], "get_or")],
            vec![fn_decl_tail(1, Visibility::Private, "main", call)],
        );
        let opt_mod = module_with_path(
            &["core", "option"],
            vec![],
            vec![fn_decl_tail(
                20,
                Visibility::Public,
                "get_or",
                int_lit(22, "0"),
            )],
        );

        let gen = RsGenerator::new();
        let out = gen
            .generate_project(&[
                (&main_mod, std::path::Path::new("src/main.bock")),
                (&opt_mod, std::path::Path::new("src/core/option.bock")),
            ])
            .unwrap();
        let by_name = |p: &str| out.files.iter().find(|f| f.path == std::path::Path::new(p));
        by_name("src/core/option.rs").expect("nested leaf module file emitted");
        let wiring = by_name("src/core.rs").expect("namespace wiring file emitted");
        assert!(
            wiring.content.contains("pub mod option;"),
            "src/core.rs must declare `pub mod option;`; got:\n{}",
            wiring.content
        );
        let main_file = by_name("src/main.rs").expect("src/main.rs emitted");
        assert!(
            main_file.content.contains("mod core;"),
            "main.rs must declare `mod core;`; got:\n{}",
            main_file.content
        );
        assert!(
            main_file
                .content
                .contains("use crate::core::option::{get_or};"),
            "main.rs must `use crate::core::option::{{get_or}};`; got:\n{}",
            main_file.content
        );
    }

    /// `fn f() { let x = if (c) { 1 } else { return 0 }  x }` — value-position
    /// `if` with a diverging else. The shared value-CF hoist lowers it to a
    /// deferred-init `let mut __bock_cf_0;` plus statement-form assignment, never
    /// `/* unsupported */`.
    fn diverging_value_if_fn() -> AIRNode {
        let then_b = block(2, vec![], Some(int_lit(3, "1")));
        let ret = node(
            5,
            NodeKind::Return {
                value: Some(Box::new(int_lit(6, "0"))),
            },
        );
        let else_b = block(4, vec![], Some(ret));
        let if_node = node(
            1,
            NodeKind::If {
                let_pattern: None,
                condition: Box::new(id_node(7, "c")),
                then_block: Box::new(then_b),
                else_block: Some(Box::new(else_b)),
            },
        );
        let let_x = node(
            10,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(bind_pat(11, "x")),
                ty: None,
                value: Box::new(if_node),
            },
        );
        let body = block(20, vec![let_x], Some(id_node(21, "x")));
        let f = node(
            30,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
                is_async: false,
                name: ident("f"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        module(vec![], vec![f])
    }

    #[test]
    fn diverging_value_if_hoists_to_stmt_form_no_unsupported() {
        let out = gen(&diverging_value_if_fn());
        assert!(
            !out.contains("/* unsupported */"),
            "diverging value-if must not emit `/* unsupported */`, got: {out}"
        );
        assert!(
            out.contains("let mut __bock_cf_0"),
            "must declare a deferred-init temp, got: {out}"
        );
        assert!(
            out.contains("__bock_cf_0 = 1"),
            "value arm must assign the temp, got: {out}"
        );
        assert!(
            out.contains("return Err") || out.contains("return 0"),
            "diverging arm must keep its return, got: {out}"
        );
    }

    // ── Rust-specific example-hardening regression tests ─────────────────────

    fn float_lit(id: u32, val: &str) -> AIRNode {
        node(
            id,
            NodeKind::Literal {
                lit: Literal::Float(val.into()),
            },
        )
    }

    fn pow(id: u32, left: AIRNode, right: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::BinaryOp {
                op: BinOp::Pow,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }

    fn type_named_node(id: u32, name: &str) -> AIRNode {
        node(
            id,
            NodeKind::TypeNamed {
                path: type_path(&[name]),
                args: vec![],
            },
        )
    }

    /// `let c: Char = 'A'` annotates the binding `char`, not the unknown `Char`
    /// (E0425). The example `type-zoo` exercises this.
    #[test]
    fn rust_char_type_annotation_lowers_to_char() {
        let ty = type_named_node(3, "Char");
        let let_node = node(
            1,
            NodeKind::LetBinding {
                pattern: Box::new(bind_pat(2, "c")),
                value: Box::new(str_lit(4, "A")),
                ty: Some(Box::new(ty)),
                is_mut: false,
            },
        );
        let mut ctx = RsEmitCtx::new();
        ctx.emit_node(&let_node).unwrap();
        assert!(ctx.buf.contains("let c: char ="), "got: {}", ctx.buf);
        assert!(!ctx.buf.contains("Char"), "got: {}", ctx.buf);
    }

    /// `2 ** 10` lowers to `i64::pow` with a `u32`-cast exponent — `pow` takes
    /// `u32`, so emitting `10_i64` is E0308. The `type-zoo` `power` case.
    #[test]
    fn rust_int_pow_casts_exponent_to_u32() {
        let expr = pow(1, int_lit(2, "2"), int_lit(3, "10"));
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&expr).unwrap();
        assert_eq!(ctx.buf, "(2_i64).pow((10_i64) as u32)", "got: {}", ctx.buf);
    }

    /// `b ** 3.0` (a Float-literal operand) lowers to `f64::powf` — `f64` has no
    /// `pow`. The exponent is cast `as f64` to admit an integer-literal exponent.
    #[test]
    fn rust_float_pow_uses_powf() {
        let expr = pow(1, id_node(2, "b"), float_lit(3, "3.0"));
        let mut ctx = RsEmitCtx::new();
        ctx.emit_expr(&expr).unwrap();
        assert_eq!(ctx.buf, "(b).powf((3.0_f64) as f64)", "got: {}", ctx.buf);
    }

    /// A function declared to return `Fn(Int) -> Int` lowers its return type to
    /// `impl Fn(i64) -> i64`, its `Fn`-typed params to `impl Fn(..) + 'static`,
    /// and its tail closure to `move |..|` — a capturing closure cannot coerce
    /// to a `fn` pointer (E0308) and the returned `impl Fn` outlives the frame
    /// (E0373/E0310). The `type-zoo` `compose_int` shape.
    #[test]
    fn rust_closure_returning_fn_uses_impl_fn_and_move() {
        let fn_ty = |id: u32| {
            node(
                id,
                NodeKind::TypeFunction {
                    params: vec![type_named_node(id + 1, "Int")],
                    ret: Box::new(type_named_node(id + 2, "Int")),
                    effects: vec![],
                },
            )
        };
        // body: `(x) => f(g(x))`
        let inner_call = node(
            30,
            NodeKind::Call {
                callee: Box::new(id_node(31, "g")),
                args: vec![AirArg {
                    label: None,
                    value: id_node(32, "x"),
                }],
                type_args: vec![],
            },
        );
        let outer_call = node(
            33,
            NodeKind::Call {
                callee: Box::new(id_node(34, "f")),
                args: vec![AirArg {
                    label: None,
                    value: inner_call,
                }],
                type_args: vec![],
            },
        );
        let lambda = node(
            35,
            NodeKind::Lambda {
                params: vec![node(
                    36,
                    NodeKind::Param {
                        pattern: Box::new(bind_pat(37, "x")),
                        ty: None,
                        default: None,
                    },
                )],
                body: Box::new(outer_call),
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("compose_int"),
                generic_params: vec![],
                params: vec![
                    node(
                        2,
                        NodeKind::Param {
                            pattern: Box::new(bind_pat(3, "f")),
                            ty: Some(Box::new(fn_ty(10))),
                            default: None,
                        },
                    ),
                    node(
                        4,
                        NodeKind::Param {
                            pattern: Box::new(bind_pat(5, "g")),
                            ty: Some(Box::new(fn_ty(15))),
                            default: None,
                        },
                    ),
                ],
                return_type: Some(Box::new(fn_ty(20))),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(40, vec![], Some(lambda))),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("-> impl Fn(i64) -> i64"),
            "return must be impl Fn, got: {out}"
        );
        assert!(
            out.contains("f: impl Fn(i64) -> i64 + 'static"),
            "params must carry + 'static, got: {out}"
        );
        assert!(
            out.contains("move |x"),
            "tail closure must move, got: {out}"
        );
        assert!(check_rs_syntax(&out), "generated rust must parse: {out}");
    }

    /// A non-`Copy` param read *inside a loop* (`for e in xs { is_cat(e, cat) }`)
    /// is moved on the first iteration, so each by-value pass must clone — even
    /// though the param appears only once textually. The `expense-tracker`
    /// `category_total` shape (param `cat`).
    #[test]
    fn rust_param_used_in_loop_is_cloned() {
        // for e in expenses { other(cat) }
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "other")),
                args: vec![AirArg {
                    label: None,
                    value: id_node(12, "cat"),
                }],
                type_args: vec![],
            },
        );
        let for_node = node(
            13,
            NodeKind::For {
                pattern: Box::new(bind_pat(14, "e")),
                iterable: Box::new(id_node(15, "expenses")),
                body: Box::new(block(16, vec![call], None)),
            },
        );
        let f = node(
            1,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Public,
                is_async: false,
                name: ident("category_total"),
                generic_params: vec![],
                params: vec![
                    typed_param_node(2, "expenses", "Expenses"),
                    typed_param_node(3, "cat", "Category"),
                ],
                return_type: None,
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(block(5, vec![for_node], None)),
            },
        );
        let out = gen(&module(vec![], vec![f]));
        assert!(out.contains("other(cat.clone())"), "got: {out}");
    }

    /// A `for` loop variable passed by value to a call and *then* read again
    /// (`is_cat(e, cat)` then `e.amount`) must clone the by-value pass — the
    /// first consumer moves the element (E0382). The `expense-tracker` `e` shape.
    #[test]
    fn rust_loop_var_passed_then_read_is_cloned() {
        let call = node(
            10,
            NodeKind::Call {
                callee: Box::new(id_node(11, "consume")),
                args: vec![AirArg {
                    label: None,
                    value: id_node(12, "e"),
                }],
                type_args: vec![],
            },
        );
        let read = node(
            17,
            NodeKind::FieldAccess {
                object: Box::new(id_node(18, "e")),
                field: ident("amount"),
            },
        );
        let for_node = node(
            13,
            NodeKind::For {
                pattern: Box::new(bind_pat(14, "e")),
                iterable: Box::new(id_node(15, "items")),
                body: Box::new(block(16, vec![call], Some(read))),
            },
        );
        let mut ctx = RsEmitCtx::new();
        ctx.emit_node(&for_node).unwrap();
        assert!(ctx.buf.contains("consume(e.clone())"), "got: {}", ctx.buf);
    }

    /// Iterating a *field access* of a binding reused after the loop
    /// (`for row in dataset.rows` then `dataset.clone()`) clones the iterable so
    /// the owner is not partially moved (E0382). The `ml-data-prep` shape.
    #[test]
    fn rust_for_over_reused_field_clones_iterable() {
        // let dataset = ...; for row in dataset.rows { } ; use(dataset)
        let for_node = node(
            13,
            NodeKind::For {
                pattern: Box::new(bind_pat(14, "row")),
                iterable: Box::new(node(
                    15,
                    NodeKind::FieldAccess {
                        object: Box::new(id_node(16, "dataset")),
                        field: ident("rows"),
                    },
                )),
                body: Box::new(block(17, vec![], None)),
            },
        );
        let use_call = node(
            20,
            NodeKind::Call {
                callee: Box::new(id_node(21, "use_it")),
                args: vec![AirArg {
                    label: None,
                    value: id_node(22, "dataset"),
                }],
                type_args: vec![],
            },
        );
        let let_dataset = node(
            1,
            NodeKind::LetBinding {
                pattern: Box::new(bind_pat(2, "dataset")),
                value: Box::new(int_lit(3, "0")),
                ty: None,
                is_mut: false,
            },
        );
        let body = block(40, vec![let_dataset, for_node, use_call], None);
        let f = fn_decl_tail_with_body(50, "main", body);
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("for row in dataset.rows.clone()"),
            "got: {out}"
        );
    }

    fn fn_decl_tail_with_body(id: u32, name: &str, body: AIRNode) -> AIRNode {
        node(
            id,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: Visibility::Private,
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

    /// A collection-typed binding interpolated into a string formats with `{:?}`
    /// — a `Vec`/`HashMap`/`HashSet` has no `Display` (E0277). The `type-zoo`
    /// `${keys}` (from `map.keys()`) shape.
    #[test]
    fn rust_interpolated_collection_binding_uses_debug_fmt() {
        // let keys = map.keys(); println("k=${keys}")
        let keys_call = node(
            10,
            NodeKind::MethodCall {
                receiver: Box::new(id_node(11, "map")),
                method: ident("keys"),
                args: vec![],
                type_args: vec![],
            },
        );
        let let_keys = node(
            1,
            NodeKind::LetBinding {
                pattern: Box::new(bind_pat(2, "keys")),
                value: Box::new(keys_call),
                ty: None,
                is_mut: false,
            },
        );
        let interp = node(
            20,
            NodeKind::Interpolation {
                parts: vec![
                    AirInterpolationPart::Literal("k=".to_string()),
                    AirInterpolationPart::Expr(Box::new(id_node(21, "keys"))),
                ],
            },
        );
        let print = node(
            22,
            NodeKind::Call {
                callee: Box::new(id_node(23, "println")),
                args: vec![AirArg {
                    label: None,
                    value: interp,
                }],
                type_args: vec![],
            },
        );
        let body = block(30, vec![let_keys, print], None);
        let f = fn_decl_tail_with_body(40, "main", body);
        let out = gen(&module(vec![], vec![f]));
        assert!(
            out.contains("\"k={:?}\""),
            "interpolated collection must use {{:?}}, got: {out}"
        );
    }
}
