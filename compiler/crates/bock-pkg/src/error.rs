//! Error types for the package manager.

/// Errors that can occur during package management operations.
#[derive(Debug, thiserror::Error)]
pub enum PkgError {
    /// Failed to parse the manifest file.
    #[error("failed to parse manifest: {0}")]
    ManifestParse(String),

    /// Failed to parse a version or version requirement.
    #[error("invalid version: {0}")]
    InvalidVersion(String),

    /// Dependency resolution failed.
    #[error("dependency resolution failed: {0}")]
    ResolutionFailed(String),

    /// Unresolvable dependency constraints.
    #[error("unresolvable dependency constraints:\n{}", format_conflicts(.0))]
    UnresolvableConstraints(Vec<String>),

    /// Package not found.
    #[error("package not found: {0}")]
    PackageNotFound(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(String),

    /// Lockfile parse error.
    #[error("failed to parse lockfile: {0}")]
    LockfileParse(String),

    /// Network request failed (transport, status, or decode error).
    #[error("network error: {0}")]
    Network(String),

    /// Downloaded artifact did not match the expected checksum.
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// SHA-256 hex expected from registry metadata.
        expected: String,
        /// SHA-256 hex actually computed from the downloaded bytes.
        actual: String,
    },
}

fn format_conflicts(conflicts: &[String]) -> String {
    conflicts
        .iter()
        .map(|c| format!("  - {c}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Result type for package operations.
pub type PkgResult<T> = Result<T, PkgError>;
