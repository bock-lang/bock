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
//! ## S5 scope
//!
//! S5 ships the *framework* + *config plumbing* + *mode gating*, fully
//! unit-tested. The per-target [`Scaffolder`] bodies are intentionally
//! **minimal stubs** (a placeholder `README.md`) — Stage S6 fills them with
//! the rich per-target output (real manifests, test-framework codegen
//! branches, formatter/linter config files, package-manager README hints).
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
/// S5 returns the minimal stub scaffolder for every target (a placeholder
/// README). S6 swaps in the rich per-target implementations.
#[must_use]
pub fn scaffolder_for(target: &str) -> Option<Box<dyn Scaffolder>> {
    let id = match target {
        "js" | "javascript" => "js",
        "ts" | "typescript" => "ts",
        "python" | "py" => "python",
        "rust" | "rs" => "rust",
        "go" | "golang" => "go",
        _ => return None,
    };
    Some(Box::new(StubScaffolder { target_id: id }))
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

/// S5 minimal scaffolder: emits a placeholder `README.md` only.
///
/// Stage S6 replaces this per target with real manifests, test files, and
/// formatter/linter configs. Kept deliberately tiny so the framework + mode
/// gating + config plumbing are what S5 proves out.
struct StubScaffolder {
    target_id: &'static str,
}

impl Scaffolder for StubScaffolder {
    fn target_id(&self) -> &'static str {
        self.target_id
    }

    fn scaffold(&self, ctx: &ScaffoldContext<'_>) -> Result<Vec<OutputFile>, ScaffoldError> {
        let name = ctx.project_name.unwrap_or("bock-project");
        let content = format!(
            "# {name} ({target})\n\
             \n\
             Project-mode output generated by `bock build` (target: {target}).\n\
             \n\
             Per-target scaffolding (manifests, test files, formatter/linter \
             configs) is filled in by a later build stage.\n",
            name = name,
            target = self.target_id,
        );
        Ok(vec![OutputFile {
            path: Path::new("README.md").to_path_buf(),
            content,
            source_map: None,
        }])
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

    #[test]
    fn stub_scaffolder_emits_placeholder_readme() {
        let cfg = TargetScaffoldConfig::default();
        let files = run_scaffolder("rust", &cfg, &[], Some("my-app")).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, Path::new("README.md"));
        assert!(files[0].content.contains("my-app"));
        assert!(files[0].content.contains("rust"));
    }

    #[test]
    fn run_scaffolder_drops_collisions_with_emitted_tree() {
        // A scaffolder file colliding with an already-emitted path is dropped.
        let cfg = TargetScaffoldConfig::default();
        let emitted = vec![OutputFile {
            path: Path::new("README.md").to_path_buf(),
            content: "already here".into(),
            source_map: None,
        }];
        let files = run_scaffolder("js", &cfg, &emitted, None).unwrap();
        assert!(
            files.is_empty(),
            "README.md collides with emitted tree and must be dropped"
        );
    }

    #[test]
    fn run_scaffolder_unknown_target_errors() {
        let cfg = TargetScaffoldConfig::default();
        let err = run_scaffolder("brainfuck", &cfg, &[], None).unwrap_err();
        assert!(matches!(err, ScaffoldError::Parse(_)));
    }
}
