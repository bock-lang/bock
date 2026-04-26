//! Build cache management in `.bock/cache/`.
//!
//! Persists build state (content hashes) between builds so that incremental
//! rebuilds can quickly determine what changed. The cache is stored as JSON
//! in the project's `.bock/cache/` directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::content_hash::HashManifest;

/// Name of the hash manifest file within the cache directory.
const MANIFEST_FILE: &str = "hash_manifest.json";

/// Manages the build cache stored in `.bock/cache/`.
#[derive(Debug, Clone)]
pub struct BuildCache {
    /// Root of the cache directory (e.g., `<project>/.bock/cache/`).
    cache_dir: PathBuf,
}

impl BuildCache {
    /// Creates a new `BuildCache` pointing at the given project root.
    ///
    /// The cache directory will be `<project_root>/.bock/cache/`.
    /// Does not create the directory; call [`ensure_cache_dir`](Self::ensure_cache_dir) first.
    #[must_use]
    pub fn new(project_root: &Path) -> Self {
        Self {
            cache_dir: project_root.join(".bock").join("cache"),
        }
    }

    /// Creates a `BuildCache` with a custom cache directory path.
    #[must_use]
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Returns the path to the cache directory.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Ensures the cache directory exists, creating it if necessary.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the directory cannot be created.
    pub fn ensure_cache_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.cache_dir)
    }

    /// Loads the cached hash manifest from disk.
    ///
    /// Returns an empty manifest if the cache file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn load_manifest(&self) -> Result<HashManifest, CacheError> {
        let path = self.cache_dir.join(MANIFEST_FILE);
        if !path.exists() {
            return Ok(HashManifest::new());
        }

        let content = fs::read_to_string(&path).map_err(CacheError::Io)?;
        serde_json::from_str(&content).map_err(CacheError::Parse)
    }

    /// Saves the hash manifest to disk.
    ///
    /// Creates the cache directory if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or the file cannot be written.
    pub fn save_manifest(&self, manifest: &HashManifest) -> Result<(), CacheError> {
        self.ensure_cache_dir().map_err(CacheError::Io)?;

        let path = self.cache_dir.join(MANIFEST_FILE);
        let content = serde_json::to_string_pretty(manifest).map_err(CacheError::Serialize)?;
        fs::write(&path, content).map_err(CacheError::Io)?;

        Ok(())
    }

    /// Clears the entire build cache.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the cache directory cannot be removed.
    pub fn clear(&self) -> io::Result<()> {
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)?;
        }
        Ok(())
    }

    /// Returns true if a cached manifest exists on disk.
    #[must_use]
    pub fn has_manifest(&self) -> bool {
        self.cache_dir.join(MANIFEST_FILE).exists()
    }
}

/// Errors that can occur during cache operations.
#[derive(Debug)]
pub enum CacheError {
    /// IO error reading or writing cache files.
    Io(io::Error),
    /// Error parsing cached JSON data.
    Parse(serde_json::Error),
    /// Error serializing data to JSON.
    Serialize(serde_json::Error),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cache I/O error: {e}"),
            Self::Parse(e) => write!(f, "cache parse error: {e}"),
            Self::Serialize(e) => write!(f, "cache serialization error: {e}"),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Parse(e) => Some(e),
            Self::Serialize(e) => Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_hash::ContentHash;

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BuildCache::new(dir.path());

        let mut manifest = HashManifest::new();
        manifest.insert("Main".to_string(), ContentHash::of_str("fn main() {}"));
        manifest.insert("Lib".to_string(), ContentHash::of_str("fn helper() {}"));

        cache.save_manifest(&manifest).unwrap();
        assert!(cache.has_manifest());

        let loaded = cache.load_manifest().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get("Main"), manifest.get("Main"));
        assert_eq!(loaded.get("Lib"), manifest.get("Lib"));
    }

    #[test]
    fn cache_empty_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BuildCache::new(dir.path());

        let manifest = cache.load_manifest().unwrap();
        assert!(manifest.is_empty());
        assert!(!cache.has_manifest());
    }

    #[test]
    fn cache_clear() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BuildCache::new(dir.path());

        let manifest = HashManifest::new();
        cache.save_manifest(&manifest).unwrap();
        assert!(cache.has_manifest());

        cache.clear().unwrap();
        assert!(!cache.has_manifest());
        assert!(!cache.cache_dir().exists());
    }

    #[test]
    fn cache_dir_path() {
        let cache = BuildCache::new(Path::new("/project"));
        assert_eq!(cache.cache_dir(), Path::new("/project/.bock/cache"));
    }

    #[test]
    fn cache_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BuildCache::new(dir.path());

        let mut m1 = HashManifest::new();
        m1.insert("A".to_string(), ContentHash::of_str("v1"));
        cache.save_manifest(&m1).unwrap();

        let mut m2 = HashManifest::new();
        m2.insert("A".to_string(), ContentHash::of_str("v2"));
        m2.insert("B".to_string(), ContentHash::of_str("v1"));
        cache.save_manifest(&m2).unwrap();

        let loaded = cache.load_manifest().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.get("A"), m2.get("A"));
    }

    #[test]
    fn ensure_cache_dir_creates_nested() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BuildCache::new(dir.path());

        assert!(!cache.cache_dir().exists());
        cache.ensure_cache_dir().unwrap();
        assert!(cache.cache_dir().exists());
    }
}
