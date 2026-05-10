use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod build;
mod cache_cmd;
mod check;
mod decision_io;
mod doc;
mod fmt;
mod inspect;
mod new;
#[path = "override.rs"]
mod override_cmd;
mod pin;
mod pkg;
mod promote;
mod repl;
mod run;
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

        /// Run only type checking (skip lint warnings).
        #[arg(long)]
        types: bool,

        /// Run only lint checks (skip type checking).
        #[arg(long)]
        lint: bool,

        /// Disable source context in diagnostic output.
        #[arg(long)]
        no_context: bool,
    },
    /// Run tests.
    Test {
        /// Optional test filter pattern.
        #[arg(long)]
        filter: Option<String>,

        /// Paths to .bock files to test. If omitted, discovers all .bock files recursively.
        files: Vec<PathBuf>,
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
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

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
            types,
            lint,
            no_context,
        } => {
            let options = check::CheckOptions {
                // If --types is passed, only run types. If --lint is passed, only run lint.
                // If neither is passed, run both.
                types: !lint,
                lint: !types,
                context: !no_context,
            };
            check::run(files, &options)?;
        }
        Command::Test { filter, files } => test::run(filter, files).await?,
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
        } => {
            let cmd = command.unwrap_or(InspectCommand::Decisions {
                runtime,
                all,
                unpinned,
                module,
                type_filter,
                json,
            });
            run_inspect(cmd)?;
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
        Command::Lsp { stdio: _ } => bock_lsp::run_stdio().await,
    }

    Ok(())
}

fn stub(name: &str) {
    println!("bock {name}: not yet implemented");
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
}

fn run_inspect(cmd: InspectCommand) -> anyhow::Result<()> {
    match cmd {
        InspectCommand::Decisions {
            runtime,
            all,
            unpinned,
            module,
            type_filter,
            json,
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
            };
            inspect::run_decisions(&options)
        }
        InspectCommand::Decision { id, json } => inspect::run_decision(&id, json),
        InspectCommand::Cache { size } => inspect::run_cache(size),
        InspectCommand::Rules { target } => inspect::run_rules(target.as_deref()),
    }
}
