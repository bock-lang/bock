//! Project-mode scaffolding framework + `bock.project` config-table parsing.
//!
//! This module is the **foundation** for project-mode output (spec §20.6.2).
//! It provides two things:
//!
//! 1. **Config parsing + validation.** [`ScaffoldConfig::from_project_toml`]
//!    parses the per-target `[targets.<T>]` (*deep*) and
//!    `[targets.<T>.scaffolding]` (*shallow*) tables in `bock.project`, and
//!    validates every supplied value against the v1-supported variant matrix
//!    in §20.6.2. An unknown value is a [`ScaffoldError`] that names the
//!    documented options for that target. Fields left unset stay `None` —
//!    target-appropriate defaults are applied later (by S6 per-target
//!    scaffolders), not here.
//!
//! 2. **The [`Scaffolder`] abstraction.** A [`Scaffolder`] takes the parsed
//!    per-target [`TargetScaffoldConfig`] plus the already-emitted module tree
//!    and produces *additional* [`OutputFile`]s — manifests, README
//!    first-contact instructions, formatter/linter configs, transpiled test
//!    files. [`scaffolder_for`] returns the per-target implementation.
//!
//! **Mode gating lives in the build driver, not here.** `bock build` runs the
//! scaffolder **only in the default (project) mode**, never under
//! `--source-only` (spec §20.6.2: source mode emits "no manifests,
//! scaffolding, or entry-point wiring").
//!
//! ## Scope (S5 framework → S6a manifests → S6b enrichment)
//!
//! S5 shipped the *framework* + *config plumbing* + *mode gating*; S6a relocated
//! the minimal run-affordance manifests here. **S6b** (this stage) fills each
//! per-target [`Scaffolder`] body with the full project-mode scaffolding:
//!
//! - a **rich manifest** referencing the selected/default **test framework**
//!   (`package.json` + `tsconfig.json`; `pyproject.toml`; `Cargo.toml`; `go.mod`),
//! - a **formatter config** where applicable (Prettier for js/ts unless
//!   `formatter=none`; Black/Ruff in `pyproject.toml`; rustfmt/gofmt are
//!   universal/always-on, no config),
//! - an **opt-in linter config** (shallow — emitted ONLY when
//!   `[targets.<T>.scaffolding].linter` is set),
//! - a **README first-contact** honoring the package-manager hint.
//!
//! Unset fields take the §20.6.2 **target-appropriate defaults**: js/ts →
//! Vitest + Prettier + npm; python → pytest + Black + pip; rust → cargo test +
//! rustfmt; go → stdlib testing + gofmt.
//!
//! **Deferred to S7** (NOT done here): the transpiled `@test` *files*
//! (Vitest/Jest/pytest/`cargo test`/`go test` test code) and the formatter-clean
//! `--check` release gate. S6b only sets up manifests/configs that *reference*
//! the framework.
//!
//! The v1-supported variant matrix (§20.6.2):
//!
//! | Target | Test framework        | Formatter                  | Linter             | Package manager  |
//! |--------|-----------------------|----------------------------|--------------------|------------------|
//! | js     | vitest (def), jest    | prettier (def), none       | eslint             | npm (def), pnpm, yarn |
//! | ts     | vitest (def), jest    | prettier (def), none       | eslint             | npm (def), pnpm, yarn |
//! | python | pytest (def), unittest| black (def), ruff, none    | ruff, pylint       | pip (def), poetry, uv |
//! | rust   | (cargo test, universal)| (rustfmt, universal)      | clippy             | (cargo only)     |
//! | go     | (stdlib, universal)   | (gofmt, universal)         | golangci-lint      | (go mod only)    |
//!
//! Rust/Go formatters and test frameworks are universal and always-on, so
//! `test_framework`/`formatter` are not user-selectable for those targets;
//! supplying them is a validation error.

use std::path::Path;

use serde::Deserialize;

use crate::generator::OutputFile;

/// Canonical target ids the config tables key on, matching
/// [`crate::profile::TargetProfile`] ids and `bock build` target selection.
pub const SCAFFOLD_TARGETS: &[&str] = &["js", "ts", "python", "rust", "go"];

/// An error parsing or validating the `bock.project` scaffolding config.
///
/// Validation errors carry the offending field, value, target, and the list of
/// documented options for that target (§20.6.2) so the build driver can render
/// a message that points the user at the valid choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaffoldError {
    /// The document was not valid TOML, or a config field had the wrong type.
    Parse(String),
    /// A config field was set to a value outside the documented options.
    UnknownValue {
        /// Target id (`"js"`, `"python"`, …).
        target: String,
        /// Config field name (`"test_framework"`, `"formatter"`, `"linter"`,
        /// `"package_manager"`).
        field: String,
        /// The unsupported value the user supplied.
        value: String,
        /// Documented valid options for this field on this target (§20.6.2).
        options: Vec<&'static str>,
    },
    /// A field was supplied for a target where it is not user-configurable
    /// (e.g. Rust/Go `formatter`/`test_framework`, which are universal).
    NotConfigurable {
        /// Target id.
        target: String,
        /// Config field name.
        field: String,
        /// Why it is not configurable, for the error message.
        reason: &'static str,
    },
}

impl std::fmt::Display for ScaffoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "bock.project parse error: {msg}"),
            Self::UnknownValue {
                target,
                field,
                value,
                options,
            } => write!(
                f,
                "unknown `{field}` value `{value}` for target `{target}`; \
                 supported options: {}",
                options.join(", ")
            ),
            Self::NotConfigurable {
                target,
                field,
                reason,
            } => write!(
                f,
                "`{field}` is not configurable for target `{target}`: {reason}"
            ),
        }
    }
}

impl std::error::Error for ScaffoldError {}

/// Typed, *validated* per-target scaffolding configuration.
///
/// Every field is `Option` — `None` means "left unset, apply the
/// target-appropriate default downstream" (S6). A `Some` value has already
/// passed the §20.6.2 matrix validation in [`ScaffoldConfig::from_project_toml`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetScaffoldConfig {
    /// Deep config: `test_framework` (`[targets.<T>]`). Affects test codegen.
    pub test_framework: Option<String>,
    /// Deep config: `formatter` (`[targets.<T>]`). Affects emitted code style.
    pub formatter: Option<String>,
    /// Deep config: `package` (`[targets.<T>]`). Overrides default package
    /// name normalization (Python `package`, etc.).
    pub package: Option<String>,
    /// Deep config: Go `module` path (`[targets.go]`).
    pub module: Option<String>,
    /// Shallow config: `linter` (`[targets.<T>.scaffolding]`). Adds a config
    /// file only.
    pub linter: Option<String>,
    /// Shallow config: `package_manager` (`[targets.<T>.scaffolding]`).
    /// Affects README hints only.
    pub package_manager: Option<String>,
}

/// Validated scaffolding configuration for all five built-in targets.
///
/// Parse with [`ScaffoldConfig::from_project_toml`]; look a target up with
/// [`ScaffoldConfig::target`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScaffoldConfig {
    js: TargetScaffoldConfig,
    ts: TargetScaffoldConfig,
    python: TargetScaffoldConfig,
    rust: TargetScaffoldConfig,
    go: TargetScaffoldConfig,
}

impl ScaffoldConfig {
    /// Returns the validated config for `target` (`"js"`, `"ts"`, `"python"`,
    /// `"rust"`, `"go"`; aliases `"javascript"`/`"typescript"`/`"py"`/`"rs"`/
    /// `"golang"` accepted), or `None` for an unknown target id.
    #[must_use]
    pub fn target(&self, target: &str) -> Option<&TargetScaffoldConfig> {
        match target {
            "js" | "javascript" => Some(&self.js),
            "ts" | "typescript" => Some(&self.ts),
            "python" | "py" => Some(&self.python),
            "rust" | "rs" => Some(&self.rust),
            "go" | "golang" => Some(&self.go),
            _ => None,
        }
    }

    /// Parses + validates the per-target tables in a `bock.project` document.
    ///
    /// Recognizes `[targets.<T>]` (deep) and `[targets.<T>.scaffolding]`
    /// (shallow) for the five built-in targets and validates each supplied
    /// value against the §20.6.2 matrix. A missing `[targets]` table (or a
    /// missing per-target table) yields all-`None` config — defaults are
    /// applied later, not here.
    ///
    /// # Errors
    ///
    /// - [`ScaffoldError::Parse`] when the document is not valid TOML or a
    ///   field has the wrong type.
    /// - [`ScaffoldError::UnknownValue`] when a field is set to a value
    ///   outside the documented options for that target.
    /// - [`ScaffoldError::NotConfigurable`] when a universal field (Rust/Go
    ///   `formatter`/`test_framework`) is supplied.
    pub fn from_project_toml(source: &str) -> Result<Self, ScaffoldError> {
        Ok(Self::from_project_toml_with_name(source)?.0)
    }

    /// Like [`Self::from_project_toml`] but also returns the `[project] name`
    /// from the same document, if present. Saves callers (the build driver) a
    /// second TOML parse and keeps the `toml`/`serde` dependency in this crate.
    ///
    /// # Errors
    ///
    /// Same as [`Self::from_project_toml`].
    pub fn from_project_toml_with_name(
        source: &str,
    ) -> Result<(Self, Option<String>), ScaffoldError> {
        let doc: ProjectDoc =
            toml::from_str(source).map_err(|e| ScaffoldError::Parse(e.to_string()))?;
        let raw = doc.targets.unwrap_or_default();
        let name = doc.project.and_then(|p| p.name);

        let config = Self {
            js: validate_target("js", raw.js)?,
            ts: validate_target("ts", raw.ts)?,
            python: validate_target("python", raw.python)?,
            rust: validate_target("rust", raw.rust)?,
            go: validate_target("go", raw.go)?,
        };
        Ok((config, name))
    }
}

// ── §20.6.2 v1 variant matrix ───────────────────────────────────────────────
//
// The codegen package owns the supported-options list per target (§20.6.2:
// "The codegen package owns the supported-options list per target; the spec
// carries the v1 matrix"). These constants are that authoritative list.

/// `test_framework` options per target. Empty = universal/not user-selectable.
fn test_framework_options(target: &str) -> &'static [&'static str] {
    match target {
        "js" | "ts" => &["vitest", "jest"],
        "python" => &["pytest", "unittest"],
        _ => &[], // rust/go: universal
    }
}

/// `formatter` options per target. Empty = universal/not user-selectable.
fn formatter_options(target: &str) -> &'static [&'static str] {
    match target {
        "js" | "ts" => &["prettier", "none"],
        "python" => &["black", "ruff", "none"],
        _ => &[], // rust/go: universal (rustfmt/gofmt always-on)
    }
}

/// `linter` (shallow) options per target.
fn linter_options(target: &str) -> &'static [&'static str] {
    match target {
        "js" | "ts" => &["eslint"],
        "python" => &["ruff", "pylint"],
        "rust" => &["clippy"],
        "go" => &["golangci-lint"],
        _ => &[],
    }
}

/// `package_manager` (shallow) options per target. Empty = single fixed
/// manager (cargo / go mod), so the field is not user-selectable.
fn package_manager_options(target: &str) -> &'static [&'static str] {
    match target {
        "js" | "ts" => &["npm", "pnpm", "yarn"],
        "python" => &["pip", "poetry", "uv"],
        _ => &[], // rust (cargo only) / go (go mod only)
    }
}

/// Validate a single field against its option list, normalizing to lowercase.
/// `None` field stays `None`. A universal field (empty option list) supplied a
/// value is [`ScaffoldError::NotConfigurable`].
fn validate_field(
    target: &str,
    field: &str,
    value: Option<String>,
    options: &[&'static str],
    universal_reason: &'static str,
) -> Result<Option<String>, ScaffoldError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if options.is_empty() {
        return Err(ScaffoldError::NotConfigurable {
            target: target.to_string(),
            field: field.to_string(),
            reason: universal_reason,
        });
    }
    let normalized = value.to_ascii_lowercase();
    if options.contains(&normalized.as_str()) {
        Ok(Some(normalized))
    } else {
        Err(ScaffoldError::UnknownValue {
            target: target.to_string(),
            field: field.to_string(),
            value,
            options: options.to_vec(),
        })
    }
}

/// Validate one target's raw deep+shallow tables into a [`TargetScaffoldConfig`].
fn validate_target(
    target: &str,
    raw: Option<RawTarget>,
) -> Result<TargetScaffoldConfig, ScaffoldError> {
    let Some(raw) = raw else {
        return Ok(TargetScaffoldConfig::default());
    };
    let scaffolding = raw.scaffolding.unwrap_or_default();

    let test_framework = validate_field(
        target,
        "test_framework",
        raw.test_framework,
        test_framework_options(target),
        "test framework is universal (cargo test / go test) and always-on",
    )?;
    let formatter = validate_field(
        target,
        "formatter",
        raw.formatter,
        formatter_options(target),
        "formatter is universal (rustfmt / gofmt) and always-on",
    )?;
    let linter = validate_field(
        target,
        "linter",
        scaffolding.linter,
        linter_options(target),
        "no linter is configurable for this target",
    )?;
    let package_manager = validate_field(
        target,
        "package_manager",
        scaffolding.package_manager,
        package_manager_options(target),
        "package manager is fixed for this target (cargo / go mod)",
    )?;

    Ok(TargetScaffoldConfig {
        test_framework,
        formatter,
        // `package` / `module` are free-form identifiers, not enum-validated.
        package: raw.package,
        module: raw.module,
        linter,
        package_manager,
    })
}

// ── Raw TOML shapes (deserialization targets) ───────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct ProjectDoc {
    project: Option<RawProject>,
    targets: Option<RawTargets>,
}

#[derive(Debug, Deserialize, Default)]
struct RawProject {
    name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RawTargets {
    js: Option<RawTarget>,
    ts: Option<RawTarget>,
    python: Option<RawTarget>,
    rust: Option<RawTarget>,
    go: Option<RawTarget>,
}

#[derive(Debug, Deserialize, Default)]
struct RawTarget {
    test_framework: Option<String>,
    formatter: Option<String>,
    package: Option<String>,
    module: Option<String>,
    scaffolding: Option<RawScaffolding>,
}

#[derive(Debug, Deserialize, Default)]
struct RawScaffolding {
    linter: Option<String>,
    package_manager: Option<String>,
}

// ── Scaffolder abstraction ──────────────────────────────────────────────────

/// Context handed to a [`Scaffolder`]: the validated per-target config plus the
/// module tree already emitted by codegen.
///
/// `emitted` is the set of source [`OutputFile`]s produced by
/// [`crate::CodeGenerator::generate_project`] for this target — the scaffolder
/// inspects it (entry file, module paths, run-affordance manifests already
/// present) to decide what additional files to add. `project_name` is the
/// `[project] name` from `bock.project` when known.
pub struct ScaffoldContext<'a> {
    /// Target id (`"js"`, `"ts"`, `"python"`, `"rust"`, `"go"`).
    pub target: &'a str,
    /// Validated per-target config (deep + shallow).
    pub config: &'a TargetScaffoldConfig,
    /// Source files already emitted for this target.
    pub emitted: &'a [OutputFile],
    /// Project name from `[project] name`, if available.
    pub project_name: Option<&'a str>,
}

/// Produces project-mode scaffolding files for one target.
///
/// A [`Scaffolder`] adds files *alongside* the emitted source tree — manifests,
/// README, formatter/linter configs, transpiled tests. It runs **only in
/// project mode** (never `--source-only`); the build driver enforces that.
///
/// Returned [`OutputFile`]s use paths relative to the target build root
/// (`build/<target>/`), the same convention as
/// [`crate::CodeGenerator::generate_project`]. A scaffolder must **not** emit a
/// file at a path the codegen tree already occupies (e.g. the run-affordance
/// `Cargo.toml`/`go.mod`/`package.json`); [`run_scaffolder`] drops any such
/// collisions defensively, but scaffolders should avoid them.
pub trait Scaffolder {
    /// The target id this scaffolder serves.
    fn target_id(&self) -> &'static str;

    /// Produce the additional scaffolding files for `ctx`.
    ///
    /// # Errors
    ///
    /// Returns [`ScaffoldError`] if scaffolding cannot be produced (e.g. an
    /// internally inconsistent config). S5 stubs do not error.
    fn scaffold(&self, ctx: &ScaffoldContext<'_>) -> Result<Vec<OutputFile>, ScaffoldError>;
}

/// Returns the [`Scaffolder`] for `target`, or `None` for an unknown target id.
///
/// Each per-target scaffolder owns the project-mode output for its target: a
/// rich manifest (the `Cargo.toml` / `go.mod` / `package.json` (+ `tsconfig.json`
/// for TS) / `pyproject.toml`) referencing the selected/default test framework,
/// a formatter config where applicable, an opt-in linter config, and a README
/// first-contact (S6b). The transpiled `@test` *files* and the formatter-clean
/// release gate are a later milestone (S7).
#[must_use]
pub fn scaffolder_for(target: &str) -> Option<Box<dyn Scaffolder>> {
    match target {
        "js" | "javascript" => Some(Box::new(JsScaffolder { target_id: "js" })),
        "ts" | "typescript" => Some(Box::new(JsScaffolder { target_id: "ts" })),
        "python" | "py" => Some(Box::new(PythonScaffolder)),
        "rust" | "rs" => Some(Box::new(RustScaffolder)),
        "go" | "golang" => Some(Box::new(GoScaffolder)),
        _ => None,
    }
}

/// Run the per-target scaffolder and return its files, dropping any that
/// collide with paths the codegen tree already emitted.
///
/// This is the single entry point the build driver calls in project mode. It
/// guards the run-affordance manifests (already emitted by codegen) against
/// accidental clobbering by a scaffolder stub.
///
/// # Errors
///
/// Propagates [`ScaffoldError`] from the scaffolder, or returns
/// [`ScaffoldError::Parse`] if `target` has no registered scaffolder.
pub fn run_scaffolder(
    target: &str,
    config: &TargetScaffoldConfig,
    emitted: &[OutputFile],
    project_name: Option<&str>,
) -> Result<Vec<OutputFile>, ScaffoldError> {
    let scaffolder = scaffolder_for(target).ok_or_else(|| {
        ScaffoldError::Parse(format!("no scaffolder registered for target `{target}`"))
    })?;
    let ctx = ScaffoldContext {
        target,
        config,
        emitted,
        project_name,
    };
    let mut files = scaffolder.scaffold(&ctx)?;
    files.retain(|f| !emitted.iter().any(|e| e.path == f.path));
    Ok(files)
}

// ── §20.6.2 defaults + shared scaffolding helpers ────────────────────────────
//
// S6b enriches the S6a run-affordance manifests into the full project-mode
// scaffolding (spec §20.6.2): a rich manifest referencing the selected/default
// test framework, a formatter config where applicable, an opt-in linter config
// (shallow), and a README first-contact honoring the package-manager hint. The
// per-target *defaults* below apply when a field is left unset in `bock.project`
// (parsed/validated in S5). The actual transpiled `@test` files and the
// formatter-clean `--check` gate are S7, not here.

/// The path codegen emits the shared Rust concurrency runtime to. Its presence
/// in the emitted tree is the signal that the crate needs `tokio` as a
/// dependency (a `Channel`/`spawn` program); a non-concurrent program emits no
/// such file and its `Cargo.toml` carries no dependencies.
const RUST_RUNTIME_REL: &str = "src/bock_runtime.rs";

/// The default project name when `[project] name` is absent from `bock.project`.
const DEFAULT_PROJECT_NAME: &str = "bock-app";

/// Normalize a project name to an npm-package-name-safe slug: lowercase,
/// non-alphanumerics collapsed to `-`, leading/trailing `-` trimmed. Empty
/// input falls back to [`DEFAULT_PROJECT_NAME`]. Used for `package.json` `name`
/// and the Go module path.
fn npm_name(project_name: Option<&str>) -> String {
    let raw = project_name.unwrap_or(DEFAULT_PROJECT_NAME);
    let slug: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        DEFAULT_PROJECT_NAME.to_string()
    } else {
        // Collapse runs of `-` to a single `-`.
        let mut out = String::with_capacity(trimmed.len());
        let mut last_dash = false;
        for c in trimmed.chars() {
            if c == '-' {
                if !last_dash {
                    out.push(c);
                }
                last_dash = true;
            } else {
                out.push(c);
                last_dash = false;
            }
        }
        out
    }
}

/// Normalize a project name to a Python/PEP 503 distribution name: like
/// [`npm_name`] but the import-safe module form uses `_` separators. This is the
/// `[project] name` for `pyproject.toml` (PEP 503 allows `-`); the importable
/// package uses `_`. Returns `(dist_name, module_name)`.
fn python_names(project_name: Option<&str>, package_override: Option<&str>) -> (String, String) {
    let dist = npm_name(project_name);
    let module = package_override
        .map(str::to_string)
        .unwrap_or_else(|| dist.replace('-', "_"));
    (dist, module)
}

/// The JS/TS test-framework default per §20.6.2 (Vitest), honoring an explicit
/// `test_framework` choice (`vitest` | `jest`).
fn js_test_framework(cfg: &TargetScaffoldConfig) -> &str {
    cfg.test_framework.as_deref().unwrap_or("vitest")
}

/// The JS/TS package-manager default per §20.6.2 (npm), honoring an explicit
/// `[scaffolding].package_manager` choice (`npm` | `pnpm` | `yarn`).
fn js_package_manager(cfg: &TargetScaffoldConfig) -> &str {
    cfg.package_manager.as_deref().unwrap_or("npm")
}

/// Whether the JS/TS formatter (Prettier) is enabled. Default is `prettier`;
/// `formatter = "none"` disables it (no `.prettierrc` emitted).
fn js_prettier_enabled(cfg: &TargetScaffoldConfig) -> bool {
    !matches!(cfg.formatter.as_deref(), Some("none"))
}

/// The Python test-framework default per §20.6.2 (pytest), honoring an explicit
/// choice (`pytest` | `unittest`).
fn py_test_framework(cfg: &TargetScaffoldConfig) -> &str {
    cfg.test_framework.as_deref().unwrap_or("pytest")
}

/// The Python formatter default per §20.6.2 (Black), honoring an explicit choice
/// (`black` | `ruff` | `none`).
fn py_formatter(cfg: &TargetScaffoldConfig) -> &str {
    cfg.formatter.as_deref().unwrap_or("black")
}

/// The Python package-manager default per §20.6.2 (pip), honoring an explicit
/// choice (`pip` | `poetry` | `uv`).
fn py_package_manager(cfg: &TargetScaffoldConfig) -> &str {
    cfg.package_manager.as_deref().unwrap_or("pip")
}

// ── JS / TS scaffolder ───────────────────────────────────────────────────────

/// JS/TS scaffolder: emits a rich `package.json` (name/version/`type:module`/
/// test script + the selected test-framework dev-dependency), a `tsconfig.json`
/// for TS, an opt-in Prettier config (`.prettierrc.json`, unless
/// `formatter=none`), an opt-in ESLint config (`.eslintrc.json`, only when
/// `[scaffolding].linter = "eslint"`), and a README first-contact honoring the
/// package-manager hint. `target_id` distinguishes `"js"` from `"ts"`: TS adds
/// `tsconfig.json` and `typescript` as a dev-dependency; the emitted run tree is
/// the same ESM `.js`.
struct JsScaffolder {
    target_id: &'static str,
}

impl JsScaffolder {
    fn is_ts(&self) -> bool {
        self.target_id == "ts"
    }

    /// Build the `package.json`: name, version, `"type":"module"` (so
    /// `node main.js` resolves the emitted ESM tree), a `test` script for the
    /// selected framework, and the matching dev-dependencies. The `"type":
    /// "module"` field is load-bearing for the toolchain run plan and must stay.
    fn package_json(&self, ctx: &ScaffoldContext<'_>) -> String {
        let name = npm_name(ctx.project_name);
        let framework = js_test_framework(ctx.config);
        let test_script = match framework {
            "jest" => "jest",
            _ => "vitest run",
        };
        let mut dev_deps: Vec<(&str, &str)> = match framework {
            "jest" => vec![("jest", "^29.0.0")],
            _ => vec![("vitest", "^2.0.0")],
        };
        if self.is_ts() {
            dev_deps.push(("typescript", "^5.0.0"));
        }
        if js_prettier_enabled(ctx.config) {
            dev_deps.push(("prettier", "^3.0.0"));
        }
        if ctx.config.linter.as_deref() == Some("eslint") {
            dev_deps.push(("eslint", "^9.0.0"));
        }
        // Stable (sorted) dev-dependency order so the manifest is deterministic.
        dev_deps.sort_by(|a, b| a.0.cmp(b.0));

        let mut s = String::new();
        s.push_str("{\n");
        s.push_str(&format!("  \"name\": \"{name}\",\n"));
        s.push_str("  \"version\": \"0.1.0\",\n");
        s.push_str("  \"private\": true,\n");
        s.push_str("  \"type\": \"module\",\n");
        s.push_str("  \"scripts\": {\n");
        s.push_str(&format!("    \"test\": \"{test_script}\"\n"));
        s.push_str("  },\n");
        s.push_str("  \"devDependencies\": {\n");
        for (i, (dep, ver)) in dev_deps.iter().enumerate() {
            let comma = if i + 1 < dev_deps.len() { "," } else { "" };
            s.push_str(&format!("    \"{dep}\": \"{ver}\"{comma}\n"));
        }
        s.push_str("  }\n");
        s.push_str("}\n");
        s
    }

    /// The `tsconfig.json` for the TS target: modern ESM output matching the
    /// `tsc main.ts` → `node main.js` run plan (NodeNext module resolution,
    /// strict). Passing files explicitly to `tsc` ignores this file, so it does
    /// not affect the harness run — it is for the user's editor/`tsc -p .`.
    fn tsconfig_json() -> String {
        "{\n  \"compilerOptions\": {\n    \"target\": \"ES2022\",\n    \
         \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \
         \"strict\": true,\n    \"esModuleInterop\": true,\n    \
         \"skipLibCheck\": true,\n    \"forceConsistentCasingInFileNames\": true\n  },\n  \
         \"include\": [\"**/*.ts\"]\n}\n"
            .to_string()
    }

    /// A baseline Prettier config (`.prettierrc.json`). Emitted unless
    /// `formatter = "none"`. Kept minimal so it agrees with Bock's emitted JS/TS
    /// style (§20.6.2 codegen-formatter agreement; the `--check` gate is S7).
    fn prettierrc() -> String {
        "{\n  \"semi\": true,\n  \"singleQuote\": false\n}\n".to_string()
    }

    /// A baseline flat ESLint config (`eslint.config.js`). Emitted only when
    /// `[scaffolding].linter = "eslint"` (shallow, opt-in). Minimal so it does
    /// not conflict with Bock's generated patterns.
    fn eslint_config() -> String {
        "export default [\n  {\n    languageOptions: { ecmaVersion: \"latest\", \
         sourceType: \"module\" },\n    rules: {}\n  }\n];\n"
            .to_string()
    }

    fn readme(&self, ctx: &ScaffoldContext<'_>) -> String {
        let name = npm_name(ctx.project_name);
        let pm = js_package_manager(ctx.config);
        let framework = js_test_framework(ctx.config);
        let lang = if self.is_ts() {
            "TypeScript"
        } else {
            "JavaScript"
        };
        let install = match pm {
            "pnpm" => "pnpm install",
            "yarn" => "yarn",
            _ => "npm install",
        };
        let test_cmd = match pm {
            "pnpm" => "pnpm test",
            "yarn" => "yarn test",
            _ => "npm test",
        };
        let run_cmd = if self.is_ts() {
            "tsc main.ts && node main.js"
        } else {
            "node main.js"
        };
        format!(
            "# {name}\n\n\
             {lang} output generated by [Bock](https://bock-lang.org) (project mode).\n\n\
             ## Run\n\n\
             ```sh\n{install}\n{run_cmd}\n```\n\n\
             ## Test\n\n\
             Tests are generated as {framework} tests:\n\n\
             ```sh\n{test_cmd}\n```\n\n\
             > Generated by `bock build`. Re-running the build regenerates this output.\n"
        )
    }
}

impl Scaffolder for JsScaffolder {
    fn target_id(&self) -> &'static str {
        self.target_id
    }

    fn scaffold(&self, ctx: &ScaffoldContext<'_>) -> Result<Vec<OutputFile>, ScaffoldError> {
        let mut files = vec![
            out_file("package.json", self.package_json(ctx)),
            out_file("README.md", self.readme(ctx)),
        ];
        if self.is_ts() {
            files.push(out_file("tsconfig.json", Self::tsconfig_json()));
        }
        if js_prettier_enabled(ctx.config) {
            files.push(out_file(".prettierrc.json", Self::prettierrc()));
        }
        if ctx.config.linter.as_deref() == Some("eslint") {
            files.push(out_file("eslint.config.js", Self::eslint_config()));
        }
        Ok(files)
    }
}

// ── Python scaffolder ────────────────────────────────────────────────────────

/// Python scaffolder: emits a `pyproject.toml` (project metadata + the selected
/// test framework's config + the Black/Ruff formatter config), an opt-in Ruff
/// linter config (only when `[scaffolding].linter` is set), and a README
/// first-contact honoring the package-manager hint (pip | Poetry | uv). The
/// per-module Python tree still runs directly (`python3 main.py`); the
/// `pyproject.toml` is inert for running and carries metadata/tooling config.
struct PythonScaffolder;

impl PythonScaffolder {
    fn pyproject(ctx: &ScaffoldContext<'_>) -> String {
        let (dist, _module) = python_names(ctx.project_name, ctx.config.package.as_deref());
        let framework = py_test_framework(ctx.config);
        let formatter = py_formatter(ctx.config);

        let mut s = String::new();
        s.push_str("[project]\n");
        s.push_str(&format!("name = \"{dist}\"\n"));
        s.push_str("version = \"0.1.0\"\n");
        s.push_str("requires-python = \">=3.9\"\n\n");

        // Test framework: pytest gets a `[tool.pytest.ini_options]`; unittest is
        // stdlib and needs no config (the README documents `python -m unittest`).
        s.push_str("[build-system]\nrequires = [\"setuptools\"]\nbuild-backend = \"setuptools.build_meta\"\n\n");
        s.push_str(&format!(
            "[dependency-groups]\n# Test framework: {framework}\ndev = [\n"
        ));
        match framework {
            "unittest" => s.push_str("]\n"),
            _ => s.push_str("  \"pytest>=8.0\",\n]\n"),
        }
        if framework == "pytest" {
            s.push_str("\n[tool.pytest.ini_options]\ntestpaths = [\".\"]\n");
        }

        // Formatter config: Black or Ruff format. `none` emits no formatter
        // section.
        match formatter {
            "black" => {
                s.push_str("\n[tool.black]\nline-length = 88\ntarget-version = [\"py39\"]\n");
            }
            "ruff" => {
                s.push_str("\n[tool.ruff]\nline-length = 88\n\n[tool.ruff.format]\nquote-style = \"double\"\n");
            }
            _ => {}
        }

        // Linter config (shallow, opt-in): ruff check or pylint.
        match ctx.config.linter.as_deref() {
            Some("ruff") => {
                // Ruff's lint section; coexists with a `[tool.ruff.format]` block.
                if formatter != "ruff" {
                    s.push_str("\n[tool.ruff]\nline-length = 88\n");
                }
                s.push_str("\n[tool.ruff.lint]\nselect = [\"E\", \"F\"]\n");
            }
            Some("pylint") => {
                s.push_str(
                    "\n[tool.pylint.main]\n# Baseline Pylint config; add rules as needed.\n",
                );
            }
            _ => {}
        }
        s
    }

    fn readme(ctx: &ScaffoldContext<'_>) -> String {
        let (dist, _module) = python_names(ctx.project_name, ctx.config.package.as_deref());
        let pm = py_package_manager(ctx.config);
        let framework = py_test_framework(ctx.config);
        let install = match pm {
            "poetry" => "poetry install",
            "uv" => "uv sync",
            _ => "pip install -e .",
        };
        let test_cmd = match (pm, framework) {
            (_, "unittest") => "python -m unittest".to_string(),
            ("poetry", _) => "poetry run pytest".to_string(),
            ("uv", _) => "uv run pytest".to_string(),
            _ => "pytest".to_string(),
        };
        format!(
            "# {dist}\n\n\
             Python output generated by [Bock](https://bock-lang.org) (project mode).\n\n\
             ## Run\n\n\
             ```sh\npython3 main.py\n```\n\n\
             ## Test\n\n\
             Tests are generated as {framework} tests:\n\n\
             ```sh\n{install}\n{test_cmd}\n```\n\n\
             > Generated by `bock build`. Re-running the build regenerates this output.\n"
        )
    }
}

impl Scaffolder for PythonScaffolder {
    fn target_id(&self) -> &'static str {
        "python"
    }

    fn scaffold(&self, ctx: &ScaffoldContext<'_>) -> Result<Vec<OutputFile>, ScaffoldError> {
        Ok(vec![
            out_file("pyproject.toml", Self::pyproject(ctx)),
            out_file("README.md", Self::readme(ctx)),
        ])
    }
}

// ── Rust scaffolder ──────────────────────────────────────────────────────────

/// Rust scaffolder: emits a `Cargo.toml` (`[package]` metadata + `[[bin]]` at
/// `src/main.rs`, `tokio` only when the emitted tree carries the concurrency
/// runtime), an opt-in `clippy.toml` (only when `[scaffolding].linter =
/// "clippy"`), and a README first-contact. rustfmt is universal/always-on, so no
/// formatter config is emitted (the formatter-clean gate is S7). cargo test is
/// the universal test framework: the README documents `cargo test` and the
/// transpiled test files (S7) land under the same crate.
struct RustScaffolder;

impl RustScaffolder {
    /// Render the `Cargo.toml`. `tokio` (with the features the concurrency
    /// runtime needs) is included only when the program uses `Channel`/`spawn`,
    /// so a non-concurrent program's crate has no dependencies and `cargo run`
    /// stays fast. The crate name is `bock_app` (a fixed, Rust-identifier-safe
    /// name — the emitted `src/main.rs` and the toolchain run plan reference the
    /// `bock_app` bin), independent of the project name.
    fn cargo_toml(needs_tokio: bool) -> String {
        let mut s = String::from(
            "[package]\n\
             name = \"bock_app\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\n\
             [[bin]]\n\
             name = \"bock_app\"\n\
             path = \"src/main.rs\"\n",
        );
        if needs_tokio {
            s.push_str(
                "\n[dependencies]\n\
                 tokio = { version = \"1\", features = [\"rt-multi-thread\", \"macros\", \"sync\", \"time\"] }\n",
            );
        }
        s
    }

    fn readme(ctx: &ScaffoldContext<'_>) -> String {
        let name = npm_name(ctx.project_name);
        format!(
            "# {name}\n\n\
             Rust output generated by [Bock](https://bock-lang.org) (project mode).\n\n\
             ## Run\n\n\
             ```sh\ncargo run\n```\n\n\
             ## Test\n\n\
             Tests are generated as `cargo test` tests:\n\n\
             ```sh\ncargo test\n```\n\n\
             Formatting is `rustfmt` (`cargo fmt`); the output is rustfmt-clean.\n\n\
             > Generated by `bock build`. Re-running the build regenerates this output.\n"
        )
    }
}

impl Scaffolder for RustScaffolder {
    fn target_id(&self) -> &'static str {
        "rust"
    }

    fn scaffold(&self, ctx: &ScaffoldContext<'_>) -> Result<Vec<OutputFile>, ScaffoldError> {
        let needs_tokio = ctx
            .emitted
            .iter()
            .any(|f| f.path == Path::new(RUST_RUNTIME_REL));
        let mut files = vec![
            out_file("Cargo.toml", Self::cargo_toml(needs_tokio)),
            out_file("README.md", Self::readme(ctx)),
        ];
        // Shallow, opt-in Clippy config.
        if ctx.config.linter.as_deref() == Some("clippy") {
            files.push(out_file(
                "clippy.toml",
                "# Baseline Clippy config; add lint thresholds as needed.\n".to_string(),
            ));
        }
        Ok(files)
    }
}

// ── Go scaffolder ────────────────────────────────────────────────────────────

/// Go scaffolder: emits a `go.mod` (module path + go version — the module path
/// honors `[targets.go] module` when set, else a slug of the project name), an
/// opt-in `.golangci.yml` (only when `[scaffolding].linter = "golangci-lint"`),
/// and a README first-contact. gofmt is universal/always-on (no config; the
/// formatter-clean gate is S7). `go test` is the universal test framework.
struct GoScaffolder;

impl GoScaffolder {
    /// The Go module path: an explicit `[targets.go] module` when set, else a
    /// name slug. A bare slug (no `/`) is a valid local module path for
    /// `go run .`. The go version is conservative (1.21) so the output builds on
    /// a wide range of installed toolchains.
    fn go_mod(ctx: &ScaffoldContext<'_>) -> String {
        let module = ctx
            .config
            .module
            .clone()
            .unwrap_or_else(|| npm_name(ctx.project_name).replace('-', "_"));
        format!("module {module}\n\ngo 1.21\n")
    }

    fn readme(ctx: &ScaffoldContext<'_>) -> String {
        let name = npm_name(ctx.project_name);
        format!(
            "# {name}\n\n\
             Go output generated by [Bock](https://bock-lang.org) (project mode).\n\n\
             ## Run\n\n\
             ```sh\ngo run .\n```\n\n\
             ## Test\n\n\
             Tests are generated as `go test` tests (stdlib `testing`):\n\n\
             ```sh\ngo test ./...\n```\n\n\
             Formatting is `gofmt` (`go fmt ./...`); the output is gofmt-clean.\n\n\
             > Generated by `bock build`. Re-running the build regenerates this output.\n"
        )
    }
}

impl Scaffolder for GoScaffolder {
    fn target_id(&self) -> &'static str {
        "go"
    }

    fn scaffold(&self, ctx: &ScaffoldContext<'_>) -> Result<Vec<OutputFile>, ScaffoldError> {
        let mut files = vec![
            out_file("go.mod", Self::go_mod(ctx)),
            out_file("README.md", Self::readme(ctx)),
        ];
        // Shallow, opt-in golangci-lint config.
        if ctx.config.linter.as_deref() == Some("golangci-lint") {
            files.push(out_file(
                ".golangci.yml",
                "# Baseline golangci-lint config; enable linters as needed.\nlinters:\n  enable:\n    - gofmt\n".to_string(),
            ));
        }
        Ok(files)
    }
}

/// Construct an [`OutputFile`] at a build-root-relative `path` with `content`
/// and no source map (scaffolding files are not transpiled source).
fn out_file(path: &str, content: String) -> OutputFile {
    OutputFile {
        path: Path::new(path).to_path_buf(),
        content,
        source_map: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Defaults / empty ────────────────────────────────────────────────────

    #[test]
    fn no_targets_table_yields_all_none() {
        let cfg = ScaffoldConfig::from_project_toml("[project]\nname = \"x\"\n").unwrap();
        for t in SCAFFOLD_TARGETS {
            let tc = cfg.target(t).unwrap();
            assert_eq!(*tc, TargetScaffoldConfig::default(), "target {t}");
        }
    }

    #[test]
    fn empty_document_yields_default() {
        let cfg = ScaffoldConfig::from_project_toml("").unwrap();
        assert_eq!(cfg, ScaffoldConfig::default());
    }

    #[test]
    fn empty_per_target_table_is_all_none() {
        let cfg = ScaffoldConfig::from_project_toml("[targets.js]\n").unwrap();
        assert_eq!(*cfg.target("js").unwrap(), TargetScaffoldConfig::default());
    }

    #[test]
    fn unset_fields_stay_none_when_some_are_set() {
        let cfg =
            ScaffoldConfig::from_project_toml("[targets.js]\ntest_framework = \"jest\"\n").unwrap();
        let js = cfg.target("js").unwrap();
        assert_eq!(js.test_framework.as_deref(), Some("jest"));
        assert!(js.formatter.is_none());
        assert!(js.linter.is_none());
        assert!(js.package_manager.is_none());
    }

    // ── Aliases ──────────────────────────────────────────────────────────────

    #[test]
    fn target_lookup_accepts_aliases() {
        let cfg =
            ScaffoldConfig::from_project_toml("[targets.python]\nformatter = \"black\"\n").unwrap();
        assert_eq!(
            cfg.target("py").unwrap().formatter.as_deref(),
            Some("black")
        );
        assert!(cfg.target("javascript").is_some());
        assert!(cfg.target("typescript").is_some());
        assert!(cfg.target("rs").is_some());
        assert!(cfg.target("golang").is_some());
        assert!(cfg.target("nope").is_none());
    }

    // ── Valid deep config, all targets ───────────────────────────────────────

    #[test]
    fn js_ts_valid_deep_and_shallow() {
        let src = r#"
[targets.js]
test_framework = "jest"
formatter = "none"
[targets.js.scaffolding]
linter = "eslint"
package_manager = "pnpm"

[targets.ts]
test_framework = "vitest"
formatter = "prettier"
[targets.ts.scaffolding]
linter = "eslint"
package_manager = "yarn"
"#;
        let cfg = ScaffoldConfig::from_project_toml(src).unwrap();
        let js = cfg.target("js").unwrap();
        assert_eq!(js.test_framework.as_deref(), Some("jest"));
        assert_eq!(js.formatter.as_deref(), Some("none"));
        assert_eq!(js.linter.as_deref(), Some("eslint"));
        assert_eq!(js.package_manager.as_deref(), Some("pnpm"));
        let ts = cfg.target("ts").unwrap();
        assert_eq!(ts.test_framework.as_deref(), Some("vitest"));
        assert_eq!(ts.formatter.as_deref(), Some("prettier"));
        assert_eq!(ts.package_manager.as_deref(), Some("yarn"));
    }

    #[test]
    fn python_valid_deep_and_shallow() {
        let src = r#"
[targets.python]
package = "my_app"
test_framework = "unittest"
formatter = "ruff"
[targets.python.scaffolding]
linter = "pylint"
package_manager = "uv"
"#;
        let cfg = ScaffoldConfig::from_project_toml(src).unwrap();
        let py = cfg.target("python").unwrap();
        assert_eq!(py.package.as_deref(), Some("my_app"));
        assert_eq!(py.test_framework.as_deref(), Some("unittest"));
        assert_eq!(py.formatter.as_deref(), Some("ruff"));
        assert_eq!(py.linter.as_deref(), Some("pylint"));
        assert_eq!(py.package_manager.as_deref(), Some("uv"));
    }

    #[test]
    fn rust_go_free_form_and_universal_linters() {
        let src = r#"
[targets.go]
module = "github.com/user/my-app"
[targets.go.scaffolding]
linter = "golangci-lint"

[targets.rust.scaffolding]
linter = "clippy"
"#;
        let cfg = ScaffoldConfig::from_project_toml(src).unwrap();
        assert_eq!(
            cfg.target("go").unwrap().module.as_deref(),
            Some("github.com/user/my-app")
        );
        assert_eq!(
            cfg.target("go").unwrap().linter.as_deref(),
            Some("golangci-lint")
        );
        assert_eq!(
            cfg.target("rust").unwrap().linter.as_deref(),
            Some("clippy")
        );
    }

    #[test]
    fn value_normalized_to_lowercase() {
        let cfg = ScaffoldConfig::from_project_toml(
            "[targets.python]\nformatter = \"Black\"\ntest_framework = \"PyTest\"\n",
        )
        .unwrap();
        let py = cfg.target("python").unwrap();
        assert_eq!(py.formatter.as_deref(), Some("black"));
        assert_eq!(py.test_framework.as_deref(), Some("pytest"));
    }

    // ── Unknown-value validation errors (one per field/target class) ─────────

    #[test]
    fn unknown_js_test_framework_errors_with_options() {
        let err = ScaffoldConfig::from_project_toml("[targets.js]\ntest_framework = \"mocha\"\n")
            .unwrap_err();
        match &err {
            ScaffoldError::UnknownValue {
                target,
                field,
                value,
                options,
            } => {
                assert_eq!(target, "js");
                assert_eq!(field, "test_framework");
                assert_eq!(value, "mocha");
                assert_eq!(options, &vec!["vitest", "jest"]);
            }
            other => panic!("expected UnknownValue, got {other:?}"),
        }
        // The Display points at the documented options.
        let msg = err.to_string();
        assert!(msg.contains("mocha"));
        assert!(msg.contains("vitest"));
        assert!(msg.contains("jest"));
    }

    #[test]
    fn unknown_python_formatter_errors() {
        let err = ScaffoldConfig::from_project_toml("[targets.python]\nformatter = \"yapf\"\n")
            .unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::UnknownValue { ref field, .. } if field == "formatter"
        ));
        let msg = err.to_string();
        assert!(msg.contains("black"));
        assert!(msg.contains("ruff"));
        assert!(msg.contains("none"));
    }

    #[test]
    fn unknown_js_formatter_errors() {
        let err = ScaffoldConfig::from_project_toml("[targets.js]\nformatter = \"standard\"\n")
            .unwrap_err();
        assert!(matches!(err, ScaffoldError::UnknownValue { .. }));
    }

    #[test]
    fn unknown_linter_errors() {
        let err =
            ScaffoldConfig::from_project_toml("[targets.js.scaffolding]\nlinter = \"jshint\"\n")
                .unwrap_err();
        match err {
            ScaffoldError::UnknownValue { field, options, .. } => {
                assert_eq!(field, "linter");
                assert_eq!(options, vec!["eslint"]);
            }
            other => panic!("expected UnknownValue, got {other:?}"),
        }
    }

    #[test]
    fn unknown_python_linter_errors() {
        let err = ScaffoldConfig::from_project_toml(
            "[targets.python.scaffolding]\nlinter = \"flake8\"\n",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::UnknownValue { ref options, .. }
                if options == &vec!["ruff", "pylint"]
        ));
    }

    #[test]
    fn unknown_package_manager_errors() {
        let err = ScaffoldConfig::from_project_toml(
            "[targets.python.scaffolding]\npackage_manager = \"conda\"\n",
        )
        .unwrap_err();
        match err {
            ScaffoldError::UnknownValue { field, options, .. } => {
                assert_eq!(field, "package_manager");
                assert_eq!(options, vec!["pip", "poetry", "uv"]);
            }
            other => panic!("expected UnknownValue, got {other:?}"),
        }
    }

    // ── NotConfigurable: universal fields ────────────────────────────────────

    #[test]
    fn rust_formatter_not_configurable() {
        let err = ScaffoldConfig::from_project_toml("[targets.rust]\nformatter = \"rustfmt\"\n")
            .unwrap_err();
        match err {
            ScaffoldError::NotConfigurable { target, field, .. } => {
                assert_eq!(target, "rust");
                assert_eq!(field, "formatter");
            }
            other => panic!("expected NotConfigurable, got {other:?}"),
        }
    }

    #[test]
    fn go_test_framework_not_configurable() {
        let err = ScaffoldConfig::from_project_toml("[targets.go]\ntest_framework = \"testify\"\n")
            .unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::NotConfigurable { ref field, .. } if field == "test_framework"
        ));
    }

    #[test]
    fn rust_package_manager_not_configurable() {
        let err = ScaffoldConfig::from_project_toml(
            "[targets.rust.scaffolding]\npackage_manager = \"cargo\"\n",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ScaffoldError::NotConfigurable { ref field, .. } if field == "package_manager"
        ));
    }

    #[test]
    fn go_package_manager_not_configurable() {
        let err = ScaffoldConfig::from_project_toml(
            "[targets.go.scaffolding]\npackage_manager = \"go\"\n",
        )
        .unwrap_err();
        assert!(matches!(err, ScaffoldError::NotConfigurable { .. }));
    }

    // ── Parse errors ─────────────────────────────────────────────────────────

    #[test]
    fn invalid_toml_is_parse_error() {
        let err = ScaffoldConfig::from_project_toml("not = valid = toml").unwrap_err();
        assert!(matches!(err, ScaffoldError::Parse(_)));
    }

    #[test]
    fn wrong_field_type_is_parse_error() {
        let err =
            ScaffoldConfig::from_project_toml("[targets.js]\ntest_framework = 42\n").unwrap_err();
        assert!(matches!(err, ScaffoldError::Parse(_)));
    }

    #[test]
    fn unrelated_sections_are_ignored() {
        // `[ai]`, `[strictness]`, etc. coexist without affecting target parsing.
        let src = r#"
[project]
name = "demo"
[strictness]
default = "development"
[ai]
provider = "anthropic"
[targets.js]
formatter = "prettier"
"#;
        let cfg = ScaffoldConfig::from_project_toml(src).unwrap();
        assert_eq!(
            cfg.target("js").unwrap().formatter.as_deref(),
            Some("prettier")
        );
    }

    // ── Scaffolder framework ─────────────────────────────────────────────────

    #[test]
    fn scaffolder_registered_for_all_targets() {
        for t in SCAFFOLD_TARGETS {
            assert!(scaffolder_for(t).is_some(), "scaffolder for {t}");
        }
        assert!(scaffolder_for("nope").is_none());
    }

    /// Find the scaffolded file at `rel` in `files`, or panic with the set of
    /// emitted paths (so a missing/renamed file fails loudly).
    fn file<'a>(files: &'a [OutputFile], rel: &str) -> &'a OutputFile {
        files
            .iter()
            .find(|f| f.path == Path::new(rel))
            .unwrap_or_else(|| {
                let paths: Vec<String> =
                    files.iter().map(|f| f.path.display().to_string()).collect();
                panic!("no scaffolded `{rel}`; emitted: {paths:?}")
            })
    }

    /// True iff any emitted file is at `rel`.
    fn has(files: &[OutputFile], rel: &str) -> bool {
        files.iter().any(|f| f.path == Path::new(rel))
    }

    // ── Name normalization ───────────────────────────────────────────────────

    #[test]
    fn npm_name_slugs_and_defaults() {
        assert_eq!(npm_name(Some("My App")), "my-app");
        assert_eq!(npm_name(Some("Foo__Bar!!")), "foo-bar");
        assert_eq!(npm_name(Some("--weird--")), "weird");
        assert_eq!(npm_name(Some("")), DEFAULT_PROJECT_NAME);
        assert_eq!(npm_name(None), DEFAULT_PROJECT_NAME);
    }

    #[test]
    fn python_names_dist_and_module() {
        let (dist, module) = python_names(Some("My App"), None);
        assert_eq!(dist, "my-app");
        assert_eq!(module, "my_app");
        // A `package` override wins for the module form.
        let (_dist, module) = python_names(Some("my-app"), Some("custom_pkg"));
        assert_eq!(module, "custom_pkg");
    }

    // ── Rust ───────────────────────────────────────────────────────────────

    #[test]
    fn rust_scaffolder_emits_cargo_toml_and_readme() {
        let cfg = TargetScaffoldConfig::default();
        let files = run_scaffolder("rust", &cfg, &[], Some("my-app")).unwrap();
        let cargo = file(&files, "Cargo.toml");
        assert!(cargo.content.contains("[package]"));
        assert!(cargo.content.contains("name = \"bock_app\""));
        assert!(cargo.content.contains("path = \"src/main.rs\""));
        // No concurrency runtime emitted ⇒ no tokio dependency.
        assert!(!cargo.content.contains("tokio"));
        let readme = file(&files, "README.md");
        assert!(readme.content.contains("# my-app"));
        assert!(readme.content.contains("cargo run"));
        assert!(readme.content.contains("cargo test"));
        // rustfmt universal/always-on: no formatter config file.
        assert!(!has(&files, "rustfmt.toml"));
        // No linter unless opted in.
        assert!(!has(&files, "clippy.toml"));
    }

    #[test]
    fn rust_scaffolder_adds_tokio_when_runtime_present() {
        let cfg = TargetScaffoldConfig::default();
        let emitted = vec![OutputFile {
            path: Path::new("src/bock_runtime.rs").to_path_buf(),
            content: "// runtime".into(),
            source_map: None,
        }];
        let files = run_scaffolder("rust", &cfg, &emitted, None).unwrap();
        let cargo = file(&files, "Cargo.toml");
        assert!(cargo.content.contains("[dependencies]"));
        assert!(cargo.content.contains("tokio"));
    }

    #[test]
    fn rust_clippy_config_only_when_opted_in() {
        let cfg = TargetScaffoldConfig {
            linter: Some("clippy".into()),
            ..Default::default()
        };
        let files = run_scaffolder("rust", &cfg, &[], None).unwrap();
        assert!(has(&files, "clippy.toml"), "clippy linter opted in");
    }

    // ── Go ───────────────────────────────────────────────────────────────────

    #[test]
    fn go_scaffolder_emits_go_mod_and_readme() {
        let cfg = TargetScaffoldConfig::default();
        let files = run_scaffolder("go", &cfg, &[], Some("my-app")).unwrap();
        let go_mod = file(&files, "go.mod");
        // Module path defaults to a module-safe slug of the project name.
        assert!(go_mod.content.contains("module my_app"));
        assert!(go_mod.content.contains("go 1.21"));
        let readme = file(&files, "README.md");
        assert!(readme.content.contains("go run ."));
        assert!(readme.content.contains("go test"));
        assert!(!has(&files, ".golangci.yml"));
    }

    #[test]
    fn go_module_path_honors_config() {
        let cfg = TargetScaffoldConfig {
            module: Some("github.com/user/my-app".into()),
            ..Default::default()
        };
        let files = run_scaffolder("go", &cfg, &[], Some("ignored")).unwrap();
        assert!(file(&files, "go.mod")
            .content
            .contains("module github.com/user/my-app"));
    }

    #[test]
    fn go_golangci_config_only_when_opted_in() {
        let cfg = TargetScaffoldConfig {
            linter: Some("golangci-lint".into()),
            ..Default::default()
        };
        let files = run_scaffolder("go", &cfg, &[], None).unwrap();
        assert!(has(&files, ".golangci.yml"));
    }

    // ── JS / TS ────────────────────────────────────────────────────────────

    #[test]
    fn js_default_scaffolding_is_vitest_prettier_npm() {
        let cfg = TargetScaffoldConfig::default();
        let files = run_scaffolder("js", &cfg, &[], Some("My App")).unwrap();
        let pkg = file(&files, "package.json");
        assert!(pkg.content.contains("\"name\": \"my-app\""));
        // `"type":"module"` is load-bearing for the `node main.js` run plan.
        assert!(pkg.content.contains("\"type\": \"module\""));
        assert!(pkg.content.contains("\"vitest\""));
        assert!(pkg.content.contains("\"vitest run\"")); // test script
        assert!(!pkg.content.contains("jest"));
        // Default formatter = prettier ⇒ config emitted.
        assert!(has(&files, ".prettierrc.json"));
        // No tsconfig for JS.
        assert!(!has(&files, "tsconfig.json"));
        // No eslint unless opted in.
        assert!(!has(&files, "eslint.config.js"));
        let readme = file(&files, "README.md");
        assert!(readme.content.contains("npm install"));
        assert!(readme.content.contains("node main.js"));
    }

    #[test]
    fn js_jest_and_none_formatter_branches() {
        let cfg = TargetScaffoldConfig {
            test_framework: Some("jest".into()),
            formatter: Some("none".into()),
            ..Default::default()
        };
        let files = run_scaffolder("js", &cfg, &[], None).unwrap();
        let pkg = file(&files, "package.json");
        assert!(pkg.content.contains("\"jest\""));
        assert!(pkg.content.contains("\"test\": \"jest\""));
        assert!(!pkg.content.contains("vitest"));
        // formatter=none ⇒ no Prettier config or dev-dep.
        assert!(!has(&files, ".prettierrc.json"));
        assert!(!pkg.content.contains("prettier"));
    }

    #[test]
    fn ts_adds_tsconfig_and_typescript_dep() {
        let cfg = TargetScaffoldConfig::default();
        let files = run_scaffolder("ts", &cfg, &[], Some("app")).unwrap();
        let pkg = file(&files, "package.json");
        assert!(pkg.content.contains("\"typescript\""));
        let ts = file(&files, "tsconfig.json");
        assert!(ts.content.contains("\"module\": \"NodeNext\""));
        assert!(ts.content.contains("\"strict\": true"));
        assert!(file(&files, "README.md")
            .content
            .contains("tsc main.ts && node main.js"));
    }

    #[test]
    fn js_eslint_and_package_manager_hint() {
        let cfg = TargetScaffoldConfig {
            linter: Some("eslint".into()),
            package_manager: Some("pnpm".into()),
            ..Default::default()
        };
        let files = run_scaffolder("js", &cfg, &[], None).unwrap();
        assert!(has(&files, "eslint.config.js"));
        let pkg = file(&files, "package.json");
        assert!(pkg.content.contains("\"eslint\""));
        // package_manager affects README only (shallow), not the manifest.
        assert!(file(&files, "README.md").content.contains("pnpm install"));
        assert!(file(&files, "README.md").content.contains("pnpm test"));
    }

    // ── Python ─────────────────────────────────────────────────────────────

    #[test]
    fn python_default_scaffolding_is_pytest_black_pip() {
        let cfg = TargetScaffoldConfig::default();
        let files = run_scaffolder("python", &cfg, &[], Some("My App")).unwrap();
        let py = file(&files, "pyproject.toml");
        assert!(py.content.contains("name = \"my-app\""));
        assert!(py.content.contains("pytest"));
        assert!(py.content.contains("[tool.pytest.ini_options]"));
        // Default formatter = black.
        assert!(py.content.contains("[tool.black]"));
        // No linter unless opted in.
        assert!(!py.content.contains("[tool.ruff.lint]"));
        assert!(!py.content.contains("[tool.pylint"));
        let readme = file(&files, "README.md");
        assert!(readme.content.contains("python3 main.py"));
        assert!(readme.content.contains("pip install"));
        assert!(readme.content.contains("pytest"));
    }

    #[test]
    fn python_unittest_ruff_uv_branches() {
        let cfg = TargetScaffoldConfig {
            test_framework: Some("unittest".into()),
            formatter: Some("ruff".into()),
            linter: Some("ruff".into()),
            package_manager: Some("uv".into()),
            ..Default::default()
        };
        let files = run_scaffolder("python", &cfg, &[], Some("app")).unwrap();
        let py = file(&files, "pyproject.toml");
        // unittest is stdlib ⇒ no pytest config/dep.
        assert!(!py.content.contains("[tool.pytest"));
        assert!(!py.content.contains("pytest>="));
        // ruff format + ruff lint.
        assert!(py.content.contains("[tool.ruff.format]"));
        assert!(py.content.contains("[tool.ruff.lint]"));
        // README: uv + unittest.
        let readme = file(&files, "README.md");
        assert!(readme.content.contains("uv sync"));
        assert!(readme.content.contains("python -m unittest"));
    }

    #[test]
    fn python_pylint_and_package_override() {
        let cfg = TargetScaffoldConfig {
            package: Some("custom_pkg".into()),
            linter: Some("pylint".into()),
            ..Default::default()
        };
        let files = run_scaffolder("python", &cfg, &[], Some("app")).unwrap();
        let py = file(&files, "pyproject.toml");
        assert!(py.content.contains("[tool.pylint.main]"));
    }

    // ── Framework / shape ──────────────────────────────────────────────────

    #[test]
    fn every_target_emits_a_readme() {
        let cfg = TargetScaffoldConfig::default();
        for t in SCAFFOLD_TARGETS {
            let files = run_scaffolder(t, &cfg, &[], Some("demo")).unwrap();
            assert!(has(&files, "README.md"), "target {t} README");
        }
    }

    #[test]
    fn run_scaffolder_drops_collisions_with_emitted_tree() {
        // A scaffolder file colliding with an already-emitted path is dropped.
        let cfg = TargetScaffoldConfig::default();
        let emitted = vec![OutputFile {
            path: Path::new("package.json").to_path_buf(),
            content: "already here".into(),
            source_map: None,
        }];
        let files = run_scaffolder("js", &cfg, &emitted, None).unwrap();
        // package.json collides and is dropped; the README still comes through.
        assert!(!has(&files, "package.json"));
        assert!(has(&files, "README.md"));
    }

    #[test]
    fn run_scaffolder_unknown_target_errors() {
        let cfg = TargetScaffoldConfig::default();
        let err = run_scaffolder("brainfuck", &cfg, &[], None).unwrap_err();
        assert!(matches!(err, ScaffoldError::Parse(_)));
    }
}
