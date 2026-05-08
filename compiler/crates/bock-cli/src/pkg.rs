//! CLI handler for `bock pkg` subcommands.

use std::env;
use std::path::Path;

use bock_pkg::commands;
use bock_pkg::install::{self, InstallOptions, CACHE_SUBDIR};
use bock_pkg::manifest::Manifest;
use bock_pkg::network::{default_registry_url, NetworkRegistry};
use bock_pkg::resolver::PackageRegistry;

use crate::{PkgCacheCommand, PkgCommand};

/// Default registry used when no `[registries].default` is configured.
const DEFAULT_REGISTRY: &str = "https://registry.bock-lang.dev/api/v1";

/// Run a package manager subcommand.
pub fn run(command: Option<PkgCommand>) -> anyhow::Result<()> {
    let Some(cmd) = command else {
        // No subcommand — show help
        println!("Usage: bock pkg <command>");
        println!("Commands: init, add, remove, tree, list, cache");
        return Ok(());
    };

    match cmd {
        PkgCommand::Init => {
            let cwd = env::current_dir()?;
            let dir_name = cwd
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("my-package")
                .to_string();
            let path = commands::init(&cwd, &dir_name)?;
            println!("Created {}", path.display());
        }
        PkgCommand::Add {
            name,
            version,
            offline,
            registry,
        } => {
            let cwd = env::current_dir()?;
            let manifest_path = commands::find_manifest(&cwd)?;
            let project_dir = manifest_path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("manifest has no parent directory"))?
                .to_path_buf();

            let registry_url = registry
                .or_else(|| default_registry_url(&project_dir))
                .unwrap_or_else(|| DEFAULT_REGISTRY.to_string());

            let cache_dir = project_dir.join(CACHE_SUBDIR);
            let net = NetworkRegistry::new(&registry_url, &cache_dir)?;

            let options = InstallOptions {
                offline,
                version_req: version.clone(),
            };
            let installed = install::install_package(&project_dir, &net, &name, &options)?;

            let source_note = if installed.source == "cache" {
                " (from cache)".to_string()
            } else {
                format!(" (from {})", installed.source)
            };
            println!(
                "Installed {} v{}{}",
                installed.name, installed.version, source_note
            );
            println!("  checksum: sha256:{}", installed.checksum);
            println!("  path:     {}", installed.install_dir.display());
        }
        PkgCommand::Remove { name } => {
            let cwd = env::current_dir()?;
            let manifest_path = commands::find_manifest(&cwd)?;
            commands::remove(&manifest_path, &name)?;
            println!("Removed {name}");
        }
        PkgCommand::Tree => {
            let cwd = env::current_dir()?;
            let manifest_path = commands::find_manifest(&cwd)?;
            let registry = PackageRegistry::new();
            let tree = commands::show_tree(&manifest_path, &registry)?;
            print!("{tree}");
        }
        PkgCommand::List => {
            let cwd = env::current_dir()?;
            let manifest_path = commands::find_manifest(&cwd)?;
            let manifest =
                Manifest::from_file(&manifest_path).map_err(|e| anyhow::anyhow!("{e}"))?;

            if manifest.dependencies.common.is_empty() && manifest.dependencies.target.is_empty() {
                println!("No dependencies.");
            } else {
                if !manifest.dependencies.common.is_empty() {
                    println!("Dependencies:");
                    for (name, spec) in &manifest.dependencies.common {
                        println!("  {name} = \"{spec}\"");
                    }
                }
                for (target, deps) in &manifest.dependencies.target {
                    println!("\nDependencies (target: {target}):");
                    for (name, spec) in deps {
                        println!("  {name} = \"{spec}\"");
                    }
                }
            }

            if !manifest.dev_dependencies.is_empty() {
                println!("\nDev dependencies:");
                for (name, spec) in &manifest.dev_dependencies {
                    println!("  {name} = \"{spec}\"");
                }
            }
        }
        PkgCommand::Cache { command } => match command {
            PkgCacheCommand::Clear => {
                let cwd = env::current_dir()?;
                let project_dir = find_project_dir(&cwd)?;
                let cache_dir = project_dir.join(CACHE_SUBDIR);
                let removed = install::clear_cache(&cache_dir)?;
                println!(
                    "Cleared {removed} tarball{} from {}",
                    if removed == 1 { "" } else { "s" },
                    cache_dir.display()
                );
            }
        },
    }

    Ok(())
}

fn find_project_dir(start: &Path) -> anyhow::Result<std::path::PathBuf> {
    // Reuse find_manifest to locate the project; fall back to cwd if absent.
    match commands::find_manifest(start) {
        Ok(manifest) => Ok(manifest
            .parent()
            .ok_or_else(|| anyhow::anyhow!("manifest has no parent directory"))?
            .to_path_buf()),
        Err(_) => Ok(start.to_path_buf()),
    }
}
