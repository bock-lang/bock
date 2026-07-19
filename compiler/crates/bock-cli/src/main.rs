use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::output::OutputFormat;

mod build;
mod cache_cmd;
mod check;
mod decision_io;
mod doc;
mod fmt;
mod inspect;
mod mcp;
mod new;
mod output;
#[path = "override.rs"]
mod override_cmd;
mod pin;
mod pkg;
mod promote;
mod repl;
mod run;
mod stdlib;
mod test;

/// The Bock programming language compiler and toolchain.
#[derive(Parser)]
#[command(name = "bock", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// All top-level subcommands.
#[derive(Subcommand)]
enum Command {
    /// Scaffold a new Bock project.
    New {
        /// Project name.
        name: String,
    },
    /// Transpile and compile a Bock project.
    Build {
        /// Target language (e.g., js, ts, python, rust, go).
        #[arg(short, long)]
        target: Option<String>,

        /// Build for all configured targets.
        #[arg(long)]
        all_targets: bool,

        /// Enable release optimizations.
        #[arg(long)]
        release: bool,

        /// Emit generated code without invoking the target compiler.
        #[arg(long)]
        source_only: bool,

        /// Use only rule-based codegen (skip AI-assisted generation).
        /// Alias: `--no-ai`.
        #[arg(long, alias = "no-ai")]
        deterministic: bool,

        /// Force production strictness for this build regardless of
        /// the project's configured default. Fails if any build-scope
        /// decision in the manifest is unpinned.
        #[arg(long)]
        strict: bool,

        /// After a successful build, pin every build-scope decision in
        /// `.bock/decisions/build/`. Intended workflow: run in
        /// development with `--pin-all`, commit the pins, then ship
        /// with production strictness.
        #[arg(long)]
        pin_all: bool,

        /// Emit source map files alongside generated code (default: on).
        #[arg(long, default_value_t = true, overrides_with = "no_source_map")]
        source_map: bool,

        /// Suppress source map file output.
        #[arg(long)]
        no_source_map: bool,
    },
    /// Execute a Bock program (interpreter by default).
    Run {
        /// Path to the entry file.
        file: Option<String>,

        /// Re-run on file changes (not yet implemented).
        #[arg(long)]
        watch: bool,

        /// Arguments passed to the Bock program (after `--`).
        #[arg(last = true)]
        program_args: Vec<String>,
    },
    /// Type-check and lint without building.
    Check {
        /// Paths to .bock files to check. If omitted, checks all .bock files in the current directory.
        files: Vec<PathBuf>,

        /// Restrict the check to specific aspects. Valid aspects: types, context.
        /// Accepts a comma-separated list (--only=types,context) and may be
        /// repeated (--only=types --only=context); omitting it runs the full check.
        #[arg(long, value_name = "ASPECT")]
        only: Vec<String>,

        /// Brief output: compact, one-line diagnostics with no source-context snippets.
        #[arg(long)]
        brief: bool,

        /// Force production strictness for the check. Without this flag the
        /// check runs at development strictness (completeness gaps are
        /// warnings); with it, completeness gaps become errors and a
        /// non-zero exit. Mirrors `bock build --strict`.
        #[arg(long)]
        strict: bool,

        /// Output format: `human` renders diagnostics to stderr; `json`
        /// emits one machine-readable JSON document on stdout.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },
    /// Run tests.
    Test {
        /// Optional test filter pattern.
        #[arg(long)]
        filter: Option<String>,

        /// Paths to .bock files to test. If omitted, discovers all .bock files recursively.
        files: Vec<PathBuf>,

        /// Output format: `human` renders per-test results; `json` emits one
        /// machine-readable JSON document on stdout.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human)]
        format: OutputFormat,
    },
    /// Format Bock source files.
    Fmt {
        /// Check formatting without modifying files.
        #[arg(long)]
        check: bool,
    },
    /// Start an interactive REPL session.
    Repl,
    /// Browse AI decisions, rule cache, and AI response cache.
    ///
    /// With no subcommand, lists build-scope decisions. Use `--runtime`
    /// for runtime decisions, `--all` for a unified view.
    Inspect {
        #[command(subcommand)]
        command: Option<InspectCommand>,

        /// Show runtime-scope decisions only (alias for `inspect decisions --runtime`).
        #[arg(long, conflicts_with = "all")]
        runtime: bool,

        /// Show both build and runtime decisions with prefixed ids.
        #[arg(long)]
        all: bool,

        /// Only list decisions that are not yet pinned.
        #[arg(long)]
        unpinned: bool,

        /// Filter by module path substring.
        #[arg(long)]
        module: Option<String>,

        /// Filter by decision type (e.g. `codegen`, `repair`, `adaptive_recovery`).
        #[arg(long = "type")]
        type_filter: Option<String>,

        /// Emit machine-readable JSON instead of the human table.
        #[arg(long)]
        json: bool,

        /// Output format: `human` prints the table; `json` emits the
        /// machine-output envelope (the legacy `--json` flag emits the bare
        /// decision array instead).
        #[arg(long, value_enum, default_value_t = OutputFormat::Human, conflicts_with = "json")]
        format: OutputFormat,
    },
    /// Pin AI decisions in the manifest so they replay deterministically.
    Pin {
        /// The decision identifier (prefixed `build:id` / `runtime:id`
        /// or bare). Omitted when using a bulk flag.
        decision: Option<String>,

        /// Pin every unpinned decision whose module path contains the
        /// given substring.
        #[arg(long = "all-in", conflicts_with_all = ["all_build", "all_runtime"])]
        all_in: Option<String>,

        /// Pin every unpinned build-scope decision.
        #[arg(long = "all-build", conflicts_with_all = ["all_in", "all_runtime"])]
        all_build: bool,

        /// Pin every unpinned runtime-scope decision.
        #[arg(long = "all-runtime", conflicts_with_all = ["all_in", "all_build"])]
        all_runtime: bool,

        /// Free-form reason recorded on pin metadata.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Clear pin metadata from a single decision.
    Unpin {
        /// The decision identifier (prefixed or bare).
        decision: String,
    },
    /// Override or promote an AI decision in the manifest.
    ///
    /// With a bare id: pin the named decision in place.
    /// With a second positional argument or `--from-file`: replace the
    /// decision's `choice` string and auto-pin.
    /// With `--promote`: copy a pinned runtime decision into the build
    /// manifest so subsequent production builds replay it
    /// deterministically.
    Override {
        /// The decision identifier to operate on.
        decision: Option<String>,

        /// Replace the decision's `choice` with this inline value.
        new_choice: Option<String>,

        /// Read the replacement choice from a file instead of the
        /// positional argument.
        #[arg(long = "from-file", conflicts_with = "new_choice")]
        from_file: Option<PathBuf>,

        /// Treat `decision` as a runtime-scope id. Required when
        /// pinning a runtime decision without promoting it.
        #[arg(long)]
        runtime: bool,

        /// Promote a pinned runtime decision to the build manifest
        /// (§10.8 promotion path). The runtime entry is marked
        /// superseded for audit but kept on disk.
        #[arg(long)]
        promote: bool,

        /// Free-form reason to record alongside the pin (e.g. a code
        /// review ticket or reviewer handle).
        #[arg(long)]
        reason: Option<String>,
    },
    /// Manage on-disk AI, decision, and rule caches.
    Cache {
        #[command(subcommand)]
        command: CacheCliCommand,
    },
    /// Analyze a project at the next strictness level (sketch → development
    /// → production). Reports issues that would fail at the next level; with
    /// `--apply`, automatically fixes simple cases and bumps the project's
    /// configured strictness default.
    Promote {
        /// Automatically apply safe fixes and update `bock.project` after a
        /// clean check. Without this flag, `bock promote` only reports.
        #[arg(long)]
        apply: bool,

        /// Report issues without modifying anything (the default).
        #[arg(long)]
        check: bool,
    },
    /// Package manager commands.
    Pkg {
        #[command(subcommand)]
        command: Option<PkgCommand>,
    },
    /// Query or interact with AI models.
    Model {
        #[command(subcommand)]
        command: Option<ModelCommand>,
    },
    /// Generate documentation.
    Doc {
        /// Path to document (file or directory, defaults to cwd).
        path: Option<String>,

        /// Output directory for generated docs (defaults to `<path>/docs`).
        #[arg(long)]
        output: Option<String>,

        /// Output format: `markdown` (default) or `html`.
        #[arg(long, default_value = "markdown")]
        format: String,
    },
    /// Start the Bock MCP server over stdio.
    ///
    /// Exposes the compiler surface (check, run, test, build, single-file
    /// cross-target conformance, inspect, diagnostic-code explanations) as
    /// Model Context Protocol tools for agentic clients. Newline-delimited
    /// JSON-RPC 2.0 on stdin/stdout; logging goes to stderr; exits 0 at EOF.
    Mcp {
        /// Use stdio transport (default and only transport in v1; accepted
        /// for convention with MCP client configurations).
        #[arg(long)]
        stdio: bool,
    },
    /// Start the Bock language server over stdio.
    Lsp {
        /// Use stdio transport (default; provided for LSP convention).
        ///
        /// `--stdio` is the universal LSP convention. We accept it for
        /// compatibility with LSP clients (VS Code, neovim, emacs
        /// lsp-mode, etc.) but stdio is already the default and only
        /// transport at v1.
        #[arg(long)]
        stdio: bool,
    },
}

/// `bock inspect` subcommands.
#[derive(Subcommand)]
enum InspectCommand {
    /// List decisions matching the given filters (default subcommand).
    Decisions {
        /// Show runtime-scope decisions only.
        #[arg(long, conflicts_with = "all")]
        runtime: bool,

        /// Show both build and runtime decisions with prefixed ids.
        #[arg(long)]
        all: bool,

        /// Only list decisions that are not yet pinned.
        #[arg(long)]
        unpinned: bool,

        /// Filter by module path substring.
        #[arg(long)]
        module: Option<String>,

        /// Filter by decision type (e.g. `codegen`, `adaptive_recovery`).
        #[arg(long = "type")]
        type_filter: Option<String>,

        /// Emit machine-readable JSON instead of the human table.
        #[arg(long)]
        json: bool,

        /// Output format: `human` prints the table; `json` emits the
        /// machine-output envelope (the legacy `--json` flag emits the bare
        /// decision array instead).
        #[arg(long, value_enum, default_value_t = OutputFormat::Human, conflicts_with = "json")]
        format: OutputFormat,
    },
    /// Show one decision in detail. Accepts prefixed or bare ids.
    Decision {
        /// The decision identifier.
        id: String,

        /// Emit JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Summarise the on-disk AI response cache.
    Cache {
        /// Always show the on-disk size even when there are no entries.
        #[arg(long)]
        size: bool,
    },
    /// List learned codegen rules.
    Rules {
        /// Show rules for a single target only.
        #[arg(long)]
        target: Option<String>,
    },
    /// Dump the lowered AIR tree for a single source file.
    ///
    /// Runs the compiler frontend (lex, parse, name resolution, AIR
    /// lowering) on one file and prints the resulting tree — indented and
    /// human-readable by default, or as a stable machine-readable JSON tree
    /// with `--json` (the shape editor tooling consumes). Exits non-zero on
    /// any frontend error; in `--json` mode errors emit a JSON error object.
    Air {
        /// The `.bock` file to lower.
        file: String,

        /// Emit the machine-readable JSON tree instead of the human view.
        #[arg(long)]
        json: bool,

        /// Output format: `json` is an alias for `--json` here — it emits
        /// the same established AIR tree document, not the envelope the
        /// other commands use.
        #[arg(long, value_enum, default_value_t = OutputFormat::Human, conflicts_with = "json")]
        format: OutputFormat,
    },
}

/// `bock cache` subcommands.
#[derive(Subcommand)]
enum CacheCliCommand {
    /// Print summary statistics for the AI cache, decision manifests,
    /// and rule cache.
    Stats,
    /// Clear a cache. By default wipes the AI response cache.
    Clear {
        /// Clear decision manifests instead of the AI cache.
        #[arg(long)]
        decisions: bool,

        /// Combined with `--decisions`, restrict the clear to the
        /// runtime manifest.
        #[arg(long)]
        runtime: bool,

        /// Combined with `--decisions`, restrict the clear to the
        /// build manifest.
        #[arg(long)]
        build: bool,

        /// Clear the local rule cache instead of the AI cache.
        #[arg(long, conflicts_with_all = ["decisions", "runtime", "build"])]
        rules: bool,
    },
}

/// Package manager subcommands.
#[derive(Subcommand)]
enum PkgCommand {
    /// Initialize a new package.
    Init,
    /// Add a dependency (downloads from the registry and updates the lockfile).
    Add {
        /// Package name.
        name: String,
        /// Version requirement (e.g. "^1.0"). Defaults to latest.
        #[arg(short, long)]
        version: Option<String>,
        /// Do not hit the network; only use cached tarballs.
        #[arg(long)]
        offline: bool,
        /// Registry URL to fetch from (overrides bock.project default).
        #[arg(long)]
        registry: Option<String>,
    },
    /// Remove a dependency.
    Remove {
        /// Package name.
        name: String,
    },
    /// Show the dependency tree.
    Tree,
    /// List dependencies.
    List,
    /// Manage the on-disk tarball cache.
    Cache {
        #[command(subcommand)]
        command: PkgCacheCommand,
    },
}

/// `bock pkg cache` subcommands.
#[derive(Subcommand)]
enum PkgCacheCommand {
    /// Remove every tarball from `.bock/cache/`.
    Clear,
}

/// Model subcommands.
#[derive(Subcommand)]
enum ModelCommand {
    /// Show current model configuration.
    Show,
    /// Set model configuration.
    Set {
        /// Configuration key.
        key: String,
        /// Configuration value.
        value: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();

    // The process exit code is decided in exactly one place: this binding,
    // seeded to success and overridden only by commands (currently `check`)
    // whose pass/fail outcome must map to a non-zero exit. Subcommand handlers
    // return their outcome rather than calling `process::exit`, keeping the
    // exit contract centralized and the handlers testable.
    let mut exit_code = ExitCode::SUCCESS;

    match cli.command {
        Command::New { name } => new::run(&name)?,
        Command::Build {
            target,
            all_targets,
            release,
            source_only,
            deterministic,
            strict,
            pin_all,
            source_map,
            no_source_map,
        } => {
            let targets = build::parse_targets(target, all_targets)?;
            let options = build::BuildOptions {
                targets,
                release,
                source_only,
                deterministic,
                strict,
                pin_all,
                source_map: source_map && !no_source_map,
            };
            build::run(&options)?;
        }
        Command::Run {
            file,
            watch,
            program_args,
        } => {
            if watch {
                println!("bock run --watch: not yet implemented");
            } else {
                run::run(file, program_args).await?;
            }
        }
        Command::Check {
            files,
            only,
            brief,
            strict,
            format,
        } => {
            match check::AspectSelection::from_raw(&only) {
                Ok(aspects) => {
                    let options = check::CheckOptions {
                        aspects,
                        brief,
                        strict,
                        format,
                    };
                    if !check::run(files, &options)?.is_clean() {
                        exit_code = ExitCode::FAILURE;
                    }
                }
                Err(unknown) => {
                    // A usage-class error our own code detects after clap has
                    // parsed argv. The pinned contract (see `crate::output`):
                    // json mode emits exactly one `outcome: "usage-error"`
                    // document on stdout; human mode keeps the stderr line.
                    // The exit code is unchanged by format.
                    let message = format!(
                        "unknown check aspect '{unknown}'. Valid aspects: {}",
                        check::Aspect::valid_list()
                    );
                    match format {
                        OutputFormat::Human => eprintln!("error: {message}"),
                        OutputFormat::Json => output::print_document(
                            &output::usage_error_document("check", "diagnostics", &message),
                        )?,
                    }
                    exit_code = ExitCode::FAILURE;
                }
            }
        }
        Command::Test {
            filter,
            files,
            format,
        } => {
            if !test::run(filter, files, format).await?.is_clean() {
                exit_code = ExitCode::FAILURE;
            }
        }
        Command::Fmt { check } => fmt::run(check)?,
        Command::Repl => repl::run().await?,
        Command::Inspect {
            command,
            runtime,
            all,
            unpinned,
            module,
            type_filter,
            json,
            format,
        } => {
            let cmd = command.unwrap_or(InspectCommand::Decisions {
                runtime,
                all,
                unpinned,
                module,
                type_filter,
                json,
                format,
            });
            // `inspect air` carries an exit-code contract (0 = lowered
            // cleanly, 1 = frontend error), mirroring `bock check`; the
            // other inspect subcommands always report `Clean`.
            if !run_inspect(cmd)?.is_clean() {
                exit_code = ExitCode::FAILURE;
            }
        }
        Command::Pin {
            decision,
            all_in,
            all_build,
            all_runtime,
            reason,
        } => {
            let options = pin::PinOptions {
                id: decision,
                all_in,
                all_build,
                all_runtime,
                reason,
            };
            pin::run_pin(&options)?;
        }
        Command::Unpin { decision } => pin::run_unpin(&decision)?,
        Command::Override {
            decision,
            new_choice,
            from_file,
            runtime,
            promote,
            reason,
        } => {
            let options = override_cmd::OverrideOptions {
                decision,
                new_choice,
                from_file,
                runtime,
                promote,
                reason,
            };
            override_cmd::run(&options)?;
        }
        Command::Cache { command } => match command {
            CacheCliCommand::Stats => cache_cmd::run_stats()?,
            CacheCliCommand::Clear {
                decisions,
                runtime,
                build,
                rules,
            } => {
                let opts = cache_cmd::ClearOptions {
                    decisions,
                    runtime,
                    build,
                    rules,
                };
                cache_cmd::run_clear(&opts)?;
            }
        },
        Command::Promote { apply, check } => {
            let options = promote::PromoteOptions {
                apply: apply && !check,
            };
            promote::run(&options)?;
        }
        Command::Pkg { command } => pkg::run(command)?,
        Command::Model { .. } => stub("model"),
        Command::Doc {
            path,
            output,
            format,
        } => doc::run(path, output, &format)?,
        Command::Mcp { stdio: _ } => mcp::run()?,
        Command::Lsp { stdio: _ } => bock_lsp::run_stdio().await,
    }

    Ok(exit_code)
}

fn stub(name: &str) {
    println!("bock {name}: not yet implemented");
}

/// Dispatch one `bock inspect` subcommand.
///
/// Returns the pass/fail outcome so `main` can map it to the process exit
/// code: `inspect air` reports `Failed` on frontend errors; every other
/// subcommand reports `Clean` (their genuine failures surface as `Err`).
fn run_inspect(cmd: InspectCommand) -> anyhow::Result<check::CheckOutcome> {
    match cmd {
        InspectCommand::Decisions {
            runtime,
            all,
            unpinned,
            module,
            type_filter,
            json,
            format,
        } => {
            let scope = if all {
                inspect::ScopeFilter::All
            } else if runtime {
                inspect::ScopeFilter::Runtime
            } else {
                inspect::ScopeFilter::Build
            };
            let options = inspect::InspectDecisionsOptions {
                scope,
                unpinned_only: unpinned,
                module_filter: module,
                type_filter,
                json,
                format,
            };
            inspect::run_decisions(&options).map(|()| check::CheckOutcome::Clean)
        }
        InspectCommand::Decision { id, json } => {
            inspect::run_decision(&id, json).map(|()| check::CheckOutcome::Clean)
        }
        InspectCommand::Cache { size } => {
            inspect::run_cache(size).map(|()| check::CheckOutcome::Clean)
        }
        InspectCommand::Rules { target } => {
            inspect::run_rules(target.as_deref()).map(|()| check::CheckOutcome::Clean)
        }
        InspectCommand::Air { file, json, format } => {
            // `--format json` is an alias for the established `--json` tree
            // contract on this subcommand (the flags conflict, so at most
            // one is set).
            inspect::run_air(
                std::path::Path::new(&file),
                json || format == OutputFormat::Json,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn lsp_accepts_stdio_flag() {
        let cli = Cli::try_parse_from(["bock", "lsp", "--stdio"])
            .expect("`bock lsp --stdio` should parse cleanly");
        assert!(matches!(cli.command, Command::Lsp { stdio: true }));
    }

    #[test]
    fn lsp_works_without_stdio_flag() {
        let cli = Cli::try_parse_from(["bock", "lsp"])
            .expect("`bock lsp` (no flag) should parse cleanly");
        assert!(matches!(cli.command, Command::Lsp { stdio: false }));
    }

    #[test]
    fn mcp_parses_with_and_without_stdio_flag() {
        let cli = Cli::try_parse_from(["bock", "mcp"]).expect("`bock mcp` should parse cleanly");
        assert!(matches!(cli.command, Command::Mcp { stdio: false }));

        let cli = Cli::try_parse_from(["bock", "mcp", "--stdio"])
            .expect("`bock mcp --stdio` should parse cleanly");
        assert!(matches!(cli.command, Command::Mcp { stdio: true }));
    }

    #[test]
    fn check_accepts_strict_flag() {
        let cli = Cli::try_parse_from(["bock", "check", "--strict", "main.bock"])
            .expect("`bock check --strict` should parse cleanly");
        assert!(matches!(cli.command, Command::Check { strict: true, .. }));
    }

    #[test]
    fn check_strict_defaults_false() {
        let cli = Cli::try_parse_from(["bock", "check", "main.bock"])
            .expect("`bock check` should parse cleanly");
        assert!(matches!(cli.command, Command::Check { strict: false, .. }));
    }

    #[test]
    fn check_format_defaults_to_human_and_accepts_json() {
        let cli = Cli::try_parse_from(["bock", "check", "main.bock"])
            .expect("`bock check` should parse cleanly");
        assert!(matches!(
            cli.command,
            Command::Check {
                format: OutputFormat::Human,
                ..
            }
        ));

        let cli = Cli::try_parse_from(["bock", "check", "--format", "json", "main.bock"])
            .expect("`bock check --format json` should parse cleanly");
        assert!(matches!(
            cli.command,
            Command::Check {
                format: OutputFormat::Json,
                ..
            }
        ));
    }

    #[test]
    fn check_format_rejects_unknown_values() {
        assert!(Cli::try_parse_from(["bock", "check", "--format", "xml", "main.bock"]).is_err());
    }

    #[test]
    fn test_command_accepts_format_json() {
        let cli = Cli::try_parse_from(["bock", "test", "--format", "json"])
            .expect("`bock test --format json` should parse cleanly");
        assert!(matches!(
            cli.command,
            Command::Test {
                format: OutputFormat::Json,
                ..
            }
        ));
    }

    #[test]
    fn inspect_format_conflicts_with_legacy_json() {
        // On the surfaces where both exist, `--format` and the legacy
        // `--json` are mutually exclusive rather than silently precedenced.
        assert!(Cli::try_parse_from(["bock", "inspect", "--json", "--format", "json"]).is_err());
        assert!(Cli::try_parse_from([
            "bock", "inspect", "air", "f.bock", "--json", "--format", "json"
        ])
        .is_err());
    }

    #[test]
    fn inspect_air_accepts_format_json_alias() {
        let cli = Cli::try_parse_from(["bock", "inspect", "air", "f.bock", "--format", "json"])
            .expect("`bock inspect air --format json` should parse cleanly");
        let Command::Inspect {
            command: Some(InspectCommand::Air { json, format, .. }),
            ..
        } = cli.command
        else {
            panic!("expected an inspect air command");
        };
        assert!(!json);
        assert_eq!(format, OutputFormat::Json);
    }
}
