//! Toolchain detection and invocation for target compilation.
//!
//! After code generation produces target-language source files, this module
//! detects installed toolchains (node, rustc, go, python3, tsc) and invokes
//! the appropriate build/validation commands per target profile.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Information about a target's toolchain requirements.
#[derive(Debug, Clone)]
pub struct ToolchainSpec {
    /// Target profile ID (e.g., "js", "rust", "go").
    pub target_id: String,
    /// Display name for error messages (e.g., "Node.js", "Rust compiler").
    pub display_name: String,
    /// Primary binary name to locate on PATH (e.g., "node", "rustc").
    pub binary_name: String,
    /// Arguments to get the toolchain version (e.g., ["--version"]).
    pub version_args: Vec<String>,
    /// Command and arguments used to validate/compile generated source.
    /// The source file path is appended as the last argument.
    pub compile_command: String,
    /// Arguments for the compile command (source path appended).
    pub compile_args: Vec<String>,
    /// Human-readable install instructions shown when toolchain is missing.
    pub install_hint: String,
}

/// Result of successfully detecting a toolchain.
#[derive(Debug, Clone)]
pub struct DetectedToolchain {
    /// Target profile ID.
    pub target_id: String,
    /// Full path to the binary, or just the binary name if resolved via PATH.
    pub binary_path: PathBuf,
    /// Version string if detection succeeded.
    pub version: Option<String>,
}

/// Result of invoking a target compilation.
#[derive(Debug)]
pub struct CompilationResult {
    /// Target profile ID.
    pub target_id: String,
    /// The command that was executed.
    pub command: String,
    /// Standard output from the command.
    pub stdout: String,
    /// Standard error from the command.
    pub stderr: String,
    /// Whether the compilation succeeded.
    pub success: bool,
}

/// Errors that can occur during toolchain operations.
#[derive(Debug)]
pub enum ToolchainError {
    /// The required toolchain binary was not found on PATH.
    NotFound {
        /// Target profile ID.
        target_id: String,
        /// Binary that was looked for.
        binary_name: String,
        /// Human-readable install instructions.
        install_hint: String,
    },
    /// The toolchain was found but the compilation/validation command failed.
    InvocationFailed {
        /// Target profile ID.
        target_id: String,
        /// The full command that was run.
        command: String,
        /// Standard output (some compilers like tsc write errors here).
        stdout: String,
        /// Standard error output.
        stderr: String,
        /// Process exit code, if available.
        exit_code: Option<i32>,
    },
    /// An I/O error occurred while invoking the toolchain.
    Io(std::io::Error),
}

impl fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolchainError::NotFound {
                target_id,
                binary_name,
                install_hint,
            } => {
                write!(
                    f,
                    "Toolchain not found for target '{target_id}': \
                     '{binary_name}' is not installed or not on PATH.\n\
                     To install: {install_hint}"
                )
            }
            ToolchainError::InvocationFailed {
                target_id,
                command,
                stdout,
                stderr,
                exit_code,
            } => {
                let diagnostic = if !stderr.is_empty() {
                    stderr
                } else {
                    stdout
                };
                write!(
                    f,
                    "Compilation failed for target '{target_id}'.\n\
                     Command: {command}\n\
                     Exit code: {}\n\
                     output:\n{diagnostic}",
                    exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".to_string())
                )
            }
            ToolchainError::Io(err) => write!(f, "I/O error during toolchain invocation: {err}"),
        }
    }
}

impl std::error::Error for ToolchainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ToolchainError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ToolchainError {
    fn from(err: std::io::Error) -> Self {
        ToolchainError::Io(err)
    }
}

/// Registry of known toolchain specifications for all supported targets.
#[derive(Debug)]
pub struct ToolchainRegistry {
    specs: HashMap<String, ToolchainSpec>,
}

impl ToolchainRegistry {
    /// Creates a new empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            specs: HashMap::new(),
        }
    }

    /// Creates a registry pre-populated with all built-in target toolchains.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(builtin_javascript_spec());
        registry.register(builtin_typescript_spec());
        registry.register(builtin_python_spec());
        registry.register(builtin_rust_spec());
        registry.register(builtin_go_spec());
        registry
    }

    /// Register a toolchain spec for a target.
    pub fn register(&mut self, spec: ToolchainSpec) {
        self.specs.insert(spec.target_id.clone(), spec);
    }

    /// Look up the toolchain spec for a target ID.
    #[must_use]
    pub fn get(&self, target_id: &str) -> Option<&ToolchainSpec> {
        self.specs.get(target_id)
    }

    /// Returns all registered target IDs.
    #[must_use]
    pub fn target_ids(&self) -> Vec<&str> {
        self.specs.keys().map(|s| s.as_str()).collect()
    }

    /// Detect whether a target's toolchain is installed.
    ///
    /// Checks for the binary on PATH and attempts to read its version.
    pub fn detect(&self, target_id: &str) -> Result<DetectedToolchain, ToolchainError> {
        let spec = self
            .specs
            .get(target_id)
            .ok_or_else(|| ToolchainError::NotFound {
                target_id: target_id.to_string(),
                binary_name: target_id.to_string(),
                install_hint: format!("No toolchain registered for target '{target_id}'"),
            })?;

        detect_toolchain(spec)
    }

    /// Detect all registered toolchains, returning found and missing.
    #[must_use]
    pub fn detect_all(&self) -> ToolchainReport {
        let mut found = Vec::new();
        let mut missing = Vec::new();

        for (target_id, spec) in &self.specs {
            match detect_toolchain(spec) {
                Ok(detected) => found.push(detected),
                Err(err) => missing.push((target_id.clone(), err)),
            }
        }

        ToolchainReport { found, missing }
    }

    /// Invoke the compilation/validation command for a target.
    ///
    /// If `source_only` is true, skips compilation and returns immediately.
    pub fn invoke(
        &self,
        target_id: &str,
        source_path: &Path,
        source_only: bool,
    ) -> Result<CompilationResult, ToolchainError> {
        if source_only {
            return Ok(CompilationResult {
                target_id: target_id.to_string(),
                command: "(source-only, compilation skipped)".to_string(),
                stdout: String::new(),
                stderr: String::new(),
                success: true,
            });
        }

        let spec = self
            .specs
            .get(target_id)
            .ok_or_else(|| ToolchainError::NotFound {
                target_id: target_id.to_string(),
                binary_name: target_id.to_string(),
                install_hint: format!("No toolchain registered for target '{target_id}'"),
            })?;

        // First ensure the toolchain is installed
        detect_toolchain(spec)?;

        // Invoke the compile command
        invoke_compile(spec, source_path)
    }
}

impl Default for ToolchainRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Report of toolchain detection across all targets.
#[derive(Debug)]
pub struct ToolchainReport {
    /// Successfully detected toolchains.
    pub found: Vec<DetectedToolchain>,
    /// Targets whose toolchain was not found, with the error.
    pub missing: Vec<(String, ToolchainError)>,
}

impl ToolchainReport {
    /// Returns true if all registered toolchains were found.
    #[must_use]
    pub fn all_found(&self) -> bool {
        self.missing.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Built-in toolchain specs
// ---------------------------------------------------------------------------

fn builtin_javascript_spec() -> ToolchainSpec {
    ToolchainSpec {
        target_id: "js".to_string(),
        display_name: "Node.js".to_string(),
        binary_name: "node".to_string(),
        version_args: vec!["--version".to_string()],
        compile_command: "node".to_string(),
        compile_args: vec!["--check".to_string()],
        install_hint: "Install Node.js from https://nodejs.org/ or via your package manager \
                        (e.g., `brew install node`, `apt install nodejs`)"
            .to_string(),
    }
}

fn builtin_typescript_spec() -> ToolchainSpec {
    ToolchainSpec {
        target_id: "ts".to_string(),
        display_name: "TypeScript compiler".to_string(),
        binary_name: "tsc".to_string(),
        version_args: vec!["--version".to_string()],
        compile_command: "tsc".to_string(),
        compile_args: vec!["--noEmit".to_string()],
        install_hint: "Install TypeScript via npm: `npm install -g typescript`".to_string(),
    }
}

fn builtin_python_spec() -> ToolchainSpec {
    ToolchainSpec {
        target_id: "python".to_string(),
        display_name: "Python 3".to_string(),
        binary_name: "python3".to_string(),
        version_args: vec!["--version".to_string()],
        compile_command: "python3".to_string(),
        compile_args: vec!["-m".to_string(), "py_compile".to_string()],
        install_hint: "Install Python 3 from https://python.org/ or via your package manager \
                        (e.g., `brew install python3`, `apt install python3`)"
            .to_string(),
    }
}

fn builtin_rust_spec() -> ToolchainSpec {
    ToolchainSpec {
        target_id: "rust".to_string(),
        display_name: "Rust compiler".to_string(),
        binary_name: "rustc".to_string(),
        version_args: vec!["--version".to_string()],
        compile_command: "rustc".to_string(),
        compile_args: vec!["--edition".to_string(), "2021".to_string()],
        install_hint: "Install Rust via rustup: https://rustup.rs/".to_string(),
    }
}

fn builtin_go_spec() -> ToolchainSpec {
    ToolchainSpec {
        target_id: "go".to_string(),
        display_name: "Go compiler".to_string(),
        binary_name: "go".to_string(),
        version_args: vec!["version".to_string()],
        compile_command: "go".to_string(),
        compile_args: vec!["vet".to_string()],
        install_hint: "Install Go from https://go.dev/dl/ or via your package manager \
                        (e.g., `brew install go`, `apt install golang`)"
            .to_string(),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Check if a binary exists on PATH and get its version.
fn detect_toolchain(spec: &ToolchainSpec) -> Result<DetectedToolchain, ToolchainError> {
    // Try to find the binary using `which` equivalent — run the version command
    let mut cmd = Command::new(&spec.binary_name);
    for arg in &spec.version_args {
        cmd.arg(arg);
    }

    let output = cmd.output().map_err(|e| {
        // Both NotFound and PermissionDenied indicate the binary isn't usable
        if e.kind() == std::io::ErrorKind::NotFound
            || e.kind() == std::io::ErrorKind::PermissionDenied
        {
            ToolchainError::NotFound {
                target_id: spec.target_id.clone(),
                binary_name: spec.binary_name.clone(),
                install_hint: spec.install_hint.clone(),
            }
        } else {
            ToolchainError::Io(e)
        }
    })?;

    let version = if output.status.success() {
        let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if v.is_empty() {
            None
        } else {
            Some(v)
        }
    } else {
        None
    };

    Ok(DetectedToolchain {
        target_id: spec.target_id.clone(),
        binary_path: PathBuf::from(&spec.binary_name),
        version,
    })
}

/// Invoke the compile/validation command for a generated source file.
fn invoke_compile(
    spec: &ToolchainSpec,
    source_path: &Path,
) -> Result<CompilationResult, ToolchainError> {
    let mut cmd = Command::new(&spec.compile_command);
    for arg in &spec.compile_args {
        cmd.arg(arg);
    }
    cmd.arg(source_path);

    let full_command = format!(
        "{} {} {}",
        spec.compile_command,
        spec.compile_args.join(" "),
        source_path.display()
    );

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound
            || e.kind() == std::io::ErrorKind::PermissionDenied
        {
            ToolchainError::NotFound {
                target_id: spec.target_id.clone(),
                binary_name: spec.compile_command.clone(),
                install_hint: spec.install_hint.clone(),
            }
        } else {
            ToolchainError::Io(e)
        }
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let success = output.status.success();

    if !success {
        return Err(ToolchainError::InvocationFailed {
            target_id: spec.target_id.clone(),
            command: full_command,
            stdout: stdout.clone(),
            stderr: stderr.clone(),
            exit_code: output.status.code(),
        });
    }

    Ok(CompilationResult {
        target_id: spec.target_id.clone(),
        command: full_command,
        stdout,
        stderr,
        success,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_with_builtins_has_all_targets() {
        let registry = ToolchainRegistry::with_builtins();
        assert!(registry.get("js").is_some());
        assert!(registry.get("ts").is_some());
        assert!(registry.get("python").is_some());
        assert!(registry.get("rust").is_some());
        assert!(registry.get("go").is_some());
        assert_eq!(registry.target_ids().len(), 5);
    }

    #[test]
    fn registry_default_equals_builtins() {
        let registry = ToolchainRegistry::default();
        assert_eq!(registry.target_ids().len(), 5);
    }

    #[test]
    fn registry_custom_spec() {
        let mut registry = ToolchainRegistry::new();
        assert!(registry.get("custom").is_none());

        registry.register(ToolchainSpec {
            target_id: "custom".to_string(),
            display_name: "Custom Lang".to_string(),
            binary_name: "customc".to_string(),
            version_args: vec!["--version".to_string()],
            compile_command: "customc".to_string(),
            compile_args: vec!["--check".to_string()],
            install_hint: "Install custom-lang from example.com".to_string(),
        });

        assert!(registry.get("custom").is_some());
        assert_eq!(registry.get("custom").unwrap().display_name, "Custom Lang");
    }

    #[test]
    fn unknown_target_returns_not_found() {
        let registry = ToolchainRegistry::with_builtins();
        let result = registry.detect("unknown_target_xyz");
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolchainError::NotFound { target_id, .. } => {
                assert_eq!(target_id, "unknown_target_xyz");
            }
            other => panic!("Expected NotFound, got: {other}"),
        }
    }

    #[test]
    fn missing_binary_returns_not_found_error() {
        let spec = ToolchainSpec {
            target_id: "fake".to_string(),
            display_name: "Fake".to_string(),
            binary_name: "definitely_not_a_real_binary_xyz_123".to_string(),
            version_args: vec!["--version".to_string()],
            compile_command: "definitely_not_a_real_binary_xyz_123".to_string(),
            compile_args: vec![],
            install_hint: "This is a test".to_string(),
        };

        let result = detect_toolchain(&spec);
        assert!(result.is_err());
        match result.unwrap_err() {
            ToolchainError::NotFound {
                target_id,
                binary_name,
                install_hint,
            } => {
                assert_eq!(target_id, "fake");
                assert_eq!(binary_name, "definitely_not_a_real_binary_xyz_123");
                assert_eq!(install_hint, "This is a test");
            }
            other => panic!("Expected NotFound, got: {other}"),
        }
    }

    #[test]
    fn not_found_error_display_includes_install_hint() {
        let err = ToolchainError::NotFound {
            target_id: "rust".to_string(),
            binary_name: "rustc".to_string(),
            install_hint: "Install via rustup".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("rust"));
        assert!(msg.contains("rustc"));
        assert!(msg.contains("Install via rustup"));
    }

    #[test]
    fn invocation_failed_error_display() {
        let err = ToolchainError::InvocationFailed {
            target_id: "js".to_string(),
            command: "node --check test.js".to_string(),
            stdout: String::new(),
            stderr: "SyntaxError: unexpected token".to_string(),
            exit_code: Some(1),
        };
        let msg = err.to_string();
        assert!(msg.contains("js"));
        assert!(msg.contains("node --check test.js"));
        assert!(msg.contains("SyntaxError"));
        assert!(msg.contains("1"));
    }

    #[test]
    fn invocation_failed_prefers_stderr_over_stdout() {
        let err = ToolchainError::InvocationFailed {
            target_id: "rust".to_string(),
            command: "rustc test.rs".to_string(),
            stdout: "ignored stdout".to_string(),
            stderr: "real error on stderr".to_string(),
            exit_code: Some(1),
        };
        let msg = err.to_string();
        assert!(msg.contains("real error on stderr"));
        assert!(!msg.contains("ignored stdout"));
    }

    #[test]
    fn invocation_failed_falls_back_to_stdout() {
        let err = ToolchainError::InvocationFailed {
            target_id: "ts".to_string(),
            command: "tsc --noEmit test.ts".to_string(),
            stdout: "test.ts(1,1): error TS2304: Cannot find name 'x'.".to_string(),
            stderr: String::new(),
            exit_code: Some(2),
        };
        let msg = err.to_string();
        assert!(msg.contains("error TS2304"));
        assert!(msg.contains("Cannot find name"));
    }

    #[test]
    fn source_only_skips_compilation() {
        let registry = ToolchainRegistry::with_builtins();
        let result = registry
            .invoke("js", Path::new("test.js"), true)
            .expect("source_only should always succeed");

        assert!(result.success);
        assert!(result.command.contains("source-only"));
        assert_eq!(result.target_id, "js");
    }

    #[test]
    fn source_only_works_for_any_target() {
        let registry = ToolchainRegistry::with_builtins();

        for target in &["js", "ts", "python", "rust", "go"] {
            let result = registry
                .invoke(target, Path::new("test.src"), true)
                .expect("source_only should succeed for all targets");
            assert!(result.success);
            assert_eq!(result.target_id, *target);
        }
    }

    #[test]
    fn invoke_unknown_target_returns_error() {
        let registry = ToolchainRegistry::with_builtins();
        let result = registry.invoke("unknown_xyz", Path::new("test.src"), false);
        assert!(result.is_err());
    }

    #[test]
    fn builtin_specs_have_correct_binaries() {
        let js = builtin_javascript_spec();
        assert_eq!(js.binary_name, "node");
        assert_eq!(js.compile_command, "node");

        let ts = builtin_typescript_spec();
        assert_eq!(ts.binary_name, "tsc");

        let py = builtin_python_spec();
        assert_eq!(py.binary_name, "python3");

        let rs = builtin_rust_spec();
        assert_eq!(rs.binary_name, "rustc");
        assert!(rs.compile_args.contains(&"--edition".to_string()));
        assert!(rs.compile_args.contains(&"2021".to_string()));

        let go = builtin_go_spec();
        assert_eq!(go.binary_name, "go");
        assert!(go.compile_args.contains(&"vet".to_string()));
    }

    #[test]
    fn detect_all_returns_report() {
        let registry = ToolchainRegistry::with_builtins();
        let report = registry.detect_all();
        // Total should match number of builtins
        assert_eq!(report.found.len() + report.missing.len(), 5);
    }

    #[test]
    fn toolchain_report_all_found() {
        // With an empty registry, all_found should be true (no missing)
        let registry = ToolchainRegistry::new();
        let report = registry.detect_all();
        assert!(report.all_found());
    }

    #[test]
    fn detect_missing_binary_via_registry() {
        let mut registry = ToolchainRegistry::new();
        registry.register(ToolchainSpec {
            target_id: "fake".to_string(),
            display_name: "Fake".to_string(),
            binary_name: "not_a_real_binary_abc_999".to_string(),
            version_args: vec!["--version".to_string()],
            compile_command: "not_a_real_binary_abc_999".to_string(),
            compile_args: vec![],
            install_hint: "Cannot install fake toolchain".to_string(),
        });

        let report = registry.detect_all();
        assert!(!report.all_found());
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].0, "fake");
    }

    #[test]
    fn invoke_with_missing_toolchain_gives_clear_error() {
        let mut registry = ToolchainRegistry::new();
        registry.register(ToolchainSpec {
            target_id: "fake".to_string(),
            display_name: "Fake Lang".to_string(),
            binary_name: "not_a_real_binary_zzz".to_string(),
            version_args: vec!["--version".to_string()],
            compile_command: "not_a_real_binary_zzz".to_string(),
            compile_args: vec!["--check".to_string()],
            install_hint: "Install from example.com".to_string(),
        });

        let result = registry.invoke("fake", Path::new("test.src"), false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not installed"));
        assert!(msg.contains("Install from example.com"));
    }
}
