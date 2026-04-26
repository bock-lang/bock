//! Implementation of the `bock new` command.
//!
//! Scaffolds a new Bock project with a standard directory layout.

use std::path::Path;

/// Create a new Bock project with the given name in the current directory.
pub fn run(name: &str) -> anyhow::Result<()> {
    create_project(Path::new(name), name)
}

/// Create a new Bock project at the given path.
fn create_project(project_dir: &Path, name: &str) -> anyhow::Result<()> {
    if project_dir.exists() {
        anyhow::bail!("directory '{name}' already exists");
    }

    // Create project directory and src/
    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir)?;

    // Generate bock.project
    let project_file = format!(
        r#"[project]
name = "{name}"
version = "0.1.0"
"#
    );
    std::fs::write(project_dir.join("bock.project"), project_file)?;

    // Generate src/main.bock
    let main_bock = r#"fn main() {
    println("Hello, world!")
}
"#;
    std::fs::write(src_dir.join("main.bock"), main_bock)?;

    // Generate .gitignore — `.bock/decisions/build/` and `.bock/rules/`
    // are committed to VCS (per §17.4 / 2026-04-22 split); the runtime
    // decision log and the AI response cache are environment-local.
    let gitignore = "\
target/
.bock/decisions/runtime/
.bock/ai-cache/
.bock/cache/
";
    std::fs::write(project_dir.join(".gitignore"), gitignore)?;

    println!("Created project {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_project_creates_scaffold() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("my-project");

        create_project(&project_dir, "my-project").unwrap();

        assert!(project_dir.join("bock.project").exists());
        assert!(project_dir.join("src/main.bock").exists());
        assert!(project_dir.join(".gitignore").exists());

        let project_content = std::fs::read_to_string(project_dir.join("bock.project")).unwrap();
        assert!(project_content.contains("name = \"my-project\""));
        assert!(project_content.contains("version = \"0.1.0\""));

        let main_content = std::fs::read_to_string(project_dir.join("src/main.bock")).unwrap();
        assert!(main_content.contains("fn main()"));
        assert!(main_content.contains("println(\"Hello, world!\")"));

        let gitignore = std::fs::read_to_string(project_dir.join(".gitignore")).unwrap();
        assert!(gitignore.contains("target/"));
        assert!(gitignore.contains(".bock/decisions/runtime/"));
        assert!(gitignore.contains(".bock/ai-cache/"));
        // Build decisions and the rule cache must NOT be ignored —
        // they are committed artifacts.
        assert!(!gitignore.contains(".bock/decisions/build"));
        assert!(!gitignore.contains(".bock/rules"));
    }

    #[test]
    fn test_new_project_fails_if_exists() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("existing");
        std::fs::create_dir(&project_dir).unwrap();

        let result = create_project(&project_dir, "existing");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }
}
