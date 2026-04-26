//! Content hashing for change detection.
//!
//! Provides SHA-256 content hashing of source files to detect changes between
//! builds. Only modules whose content hash differs from the cached hash need
//! to be recompiled.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

/// A hex-encoded SHA-256 content hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ContentHash(pub String);

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl ContentHash {
    /// Computes the SHA-256 hash of the given content bytes.
    #[must_use]
    pub fn of_bytes(content: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let result = hasher.finalize();
        Self(hex_encode(&result))
    }

    /// Computes the SHA-256 hash of the given string content.
    #[must_use]
    pub fn of_str(content: &str) -> Self {
        Self::of_bytes(content.as_bytes())
    }

    /// Computes the SHA-256 hash of a file's contents.
    ///
    /// # Errors
    ///
    /// Returns an IO error if the file cannot be read.
    pub fn of_file(path: &Path) -> io::Result<Self> {
        let content = fs::read(path)?;
        Ok(Self::of_bytes(&content))
    }
}

/// A map from module identifiers to their content hashes.
///
/// Used to compare current file state against a cached build state
/// to determine which modules have changed.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HashManifest {
    /// Map from module ID to content hash.
    pub hashes: HashMap<String, ContentHash>,
}

impl HashManifest {
    /// Creates a new empty hash manifest.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or updates a module's content hash.
    pub fn insert(&mut self, module_id: String, hash: ContentHash) {
        self.hashes.insert(module_id, hash);
    }

    /// Returns the stored hash for a module, if any.
    #[must_use]
    pub fn get(&self, module_id: &str) -> Option<&ContentHash> {
        self.hashes.get(module_id)
    }

    /// Computes which modules have changed between this manifest (old) and another (new).
    ///
    /// Returns the set of module IDs that are new, removed, or have different hashes.
    #[must_use]
    pub fn changed_modules(&self, current: &HashManifest) -> Vec<String> {
        let mut changed = Vec::new();

        // Modules that are new or have changed content
        for (module_id, new_hash) in &current.hashes {
            match self.hashes.get(module_id) {
                Some(old_hash) if old_hash == new_hash => {}
                _ => changed.push(module_id.clone()),
            }
        }

        // Modules that were removed
        for module_id in self.hashes.keys() {
            if !current.hashes.contains_key(module_id) {
                changed.push(module_id.clone());
            }
        }

        changed
    }

    /// Returns the number of entries in the manifest.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// Returns true if the manifest is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }
}

/// Hex-encode a byte slice.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_deterministic() {
        let h1 = ContentHash::of_str("hello world");
        let h2 = ContentHash::of_str("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_differs_for_different_content() {
        let h1 = ContentHash::of_str("hello");
        let h2 = ContentHash::of_str("world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_is_hex_encoded_sha256() {
        let h = ContentHash::of_str("");
        // SHA-256 of empty string is well-known
        assert_eq!(
            h.0,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(h.0.len(), 64);
    }

    #[test]
    fn hash_manifest_changed_modules() {
        let mut old = HashManifest::new();
        old.insert("A".to_string(), ContentHash::of_str("v1"));
        old.insert("B".to_string(), ContentHash::of_str("v1"));
        old.insert("C".to_string(), ContentHash::of_str("v1"));

        let mut new = HashManifest::new();
        new.insert("A".to_string(), ContentHash::of_str("v1")); // unchanged
        new.insert("B".to_string(), ContentHash::of_str("v2")); // changed
        new.insert("D".to_string(), ContentHash::of_str("v1")); // added

        let mut changed = old.changed_modules(&new);
        changed.sort();
        assert_eq!(changed, vec!["B", "C", "D"]);
    }

    #[test]
    fn hash_manifest_empty() {
        let old = HashManifest::new();
        let new = HashManifest::new();
        assert!(old.changed_modules(&new).is_empty());
    }

    #[test]
    fn hash_manifest_all_new() {
        let old = HashManifest::new();
        let mut new = HashManifest::new();
        new.insert("A".to_string(), ContentHash::of_str("v1"));
        new.insert("B".to_string(), ContentHash::of_str("v1"));

        let mut changed = old.changed_modules(&new);
        changed.sort();
        assert_eq!(changed, vec!["A", "B"]);
    }

    #[test]
    fn hash_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bock");
        fs::write(&path, "fn main() {}").unwrap();

        let h1 = ContentHash::of_file(&path).unwrap();
        let h2 = ContentHash::of_str("fn main() {}");
        assert_eq!(h1, h2);
    }
}
