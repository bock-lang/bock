//! Implementation of the `bock fmt` command.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Run the formatter on the given files (or current directory).
pub fn run(check: bool) -> Result<()> {
    let files = discover_bock_files()?;

    if files.is_empty() {
        println!("No .bock files found");
        return Ok(());
    }

    let mut any_changed = false;

    for path in &files {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        let filename = path.to_string_lossy().to_string();
        let result = bock_fmt::format_source(&source, &filename);

        if result.changed {
            any_changed = true;
            if check {
                println!("Would reformat: {}", path.display());
            } else {
                std::fs::write(path, &result.output)
                    .with_context(|| format!("failed to write {}", path.display()))?;
                println!("Formatted: {}", path.display());
            }
        }
    }

    if check && any_changed {
        let count = files
            .iter()
            .filter(|p| {
                let src = std::fs::read_to_string(p).unwrap_or_default();
                let fname = p.to_string_lossy().to_string();
                bock_fmt::format_source(&src, &fname).changed
            })
            .count();
        println!("{count} file(s) would be reformatted");
        std::process::exit(1);
    }

    if !check && !any_changed {
        println!("All files already formatted");
    }

    Ok(())
}

/// Discover all `.bock` files in the current directory (recursively).
fn discover_bock_files() -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_bock_files(&std::env::current_dir()?, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_bock_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_bock_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "bock") {
            out.push(path);
        }
    }
    Ok(())
}
