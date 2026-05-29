//! Content-addressed AI response cache (§17.8).
//!
//! Each entry is a JSON-serialized provider response keyed by the
//! SHA-256 of the canonical JSON of the originating request. The cache
//! lives at `.bock/ai-cache/` and is sharded git-style:
//! `.bock/ai-cache/{hash[0..2]}/{hash}.json`.
//!
//! "Canonical" here means: serialize the request through
//! [`serde_json::Value`] before stringifying. `serde_json::Value`'s
//! object representation is backed by a [`BTreeMap`](std::collections::BTreeMap)
//! (when the optional `preserve_order` feature is off, which is the
//! default), so map keys are emitted in sorted order — which is what
//! makes the hash stable across `HashMap` iteration order, struct
//! field reordering, and language-version differences in JSON
//! formatting.
//!
//! Cached responses are treated as **pinned** by callers: replaying a
//! cache hit is by construction deterministic, so [`CachingProvider`](
//! crate::caching_provider::CachingProvider) reports a `from_cache: true`
//! signal that the decision-recording layer uses to set
//! [`Decision::pinned`](crate::decision::Decision::pinned).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Errors produced by the AI response cache.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Filesystem I/O failed.
    #[error("ai-cache I/O error: {0}")]
    Io(#[from] io::Error),
    /// JSON serialization failed (request or response).
    #[error("ai-cache serialize error: {0}")]
    Serialize(serde_json::Error),
}

/// Summary statistics for the cache directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Number of cached entries on disk.
    pub entries: usize,
    /// Sum of cached entry file sizes, in bytes.
    pub total_bytes: u64,
}

/// Content-addressed cache backed by `.bock/ai-cache/`.
#[derive(Debug, Clone)]
pub struct AiCache {
    root: PathBuf,
}

impl AiCache {
    /// Creates a cache rooted at `<project_root>/.bock/ai-cache/`.
    ///
    /// Does not create the directory; it is materialised on first
    /// [`put`](Self::put).
    #[must_use]
    pub fn new(project_root: &Path) -> Self {
        Self {
            root: project_root.join(".bock").join("ai-cache"),
        }
    }

    /// Creates a cache rooted at an explicit directory.
    #[must_use]
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    /// Path to the cache root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the cached response for `request`, or [`None`] on miss.
    ///
    /// On a serialization or I/O error the entry is treated as a miss
    /// — the cache is best-effort and never blocks a fresh provider
    /// call. To inspect the underlying error explicitly use
    /// [`get_strict`](Self::get_strict).
    #[must_use]
    pub fn get<R: Serialize, S: DeserializeOwned>(&self, request: &R) -> Option<S> {
        self.get_strict(request).ok().flatten()
    }

    /// Like [`get`](Self::get) but surfaces serialization or I/O errors
    /// instead of silently treating them as misses.
    ///
    /// # Errors
    /// Returns [`CacheError`] when request hashing, file I/O, or
    /// response deserialization fails.
    pub fn get_strict<R: Serialize, S: DeserializeOwned>(
        &self,
        request: &R,
    ) -> Result<Option<S>, CacheError> {
        let key = compute_key(request)?;
        let path = self.path_for_key(&key);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        let value = serde_json::from_slice(&bytes).map_err(CacheError::Serialize)?;
        Ok(Some(value))
    }

    /// Stores `response` keyed by the canonical hash of `request`.
    ///
    /// Creates intermediate shard directories on demand. Existing
    /// entries with the same key are overwritten — the cache is
    /// content-addressed, so the new write would only differ from the
    /// old if the response itself changed for the same request.
    ///
    /// # Errors
    /// Returns [`CacheError`] on serialization or I/O failure.
    pub fn put<R: Serialize, S: Serialize>(
        &self,
        request: &R,
        response: &S,
    ) -> Result<(), CacheError> {
        let key = compute_key(request)?;
        let path = self.path_for_key(&key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec(response).map_err(CacheError::Serialize)?;
        fs::write(&path, bytes)?;
        Ok(())
    }

    /// Returns true if a cached entry exists for `request`.
    #[must_use]
    pub fn contains<R: Serialize>(&self, request: &R) -> bool {
        match compute_key(request) {
            Ok(key) => self.path_for_key(&key).exists(),
            Err(_) => false,
        }
    }

    /// Removes every cached entry by deleting the cache root.
    ///
    /// # Errors
    /// Returns an I/O error if the directory exists but cannot be
    /// removed.
    pub fn clear(&self) -> io::Result<()> {
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        Ok(())
    }

    /// Walks the cache directory and counts entries + bytes.
    ///
    /// # Errors
    /// Returns an I/O error if directory traversal fails.
    pub fn stats(&self) -> io::Result<CacheStats> {
        let mut stats = CacheStats::default();
        if self.root.exists() {
            walk(&self.root, &mut stats)?;
        }
        Ok(stats)
    }

    fn path_for_key(&self, key: &str) -> PathBuf {
        let shard = &key[..2];
        self.root.join(shard).join(format!("{key}.json"))
    }
}

/// Computes the SHA-256 of the canonical JSON of `request`.
///
/// Goes through [`serde_json::Value`] so map keys are sorted
/// regardless of `HashMap` iteration order in the source type.
///
/// # Errors
/// Returns [`CacheError::Serialize`] when `request` cannot be
/// serialized to JSON.
pub fn compute_key<R: Serialize>(request: &R) -> Result<String, CacheError> {
    let value = serde_json::to_value(request).map_err(CacheError::Serialize)?;
    let canonical = serde_json::to_vec(&value).map_err(CacheError::Serialize)?;
    let mut hasher = Sha256::new();
    hasher.update(&canonical);
    Ok(hex_encode(&hasher.finalize()))
}

fn walk(dir: &Path, stats: &mut CacheStats) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            walk(&entry.path(), stats)?;
        } else if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
            stats.entries += 1;
            stats.total_bytes = stats.total_bytes.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Req {
        kind: String,
        params: HashMap<String, String>,
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Resp {
        body: String,
    }

    fn req(kind: &str) -> Req {
        let mut params = HashMap::new();
        params.insert("a".into(), "1".into());
        params.insert("b".into(), "2".into());
        Req {
            kind: kind.into(),
            params,
        }
    }

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AiCache::new(dir.path());
        let r = req("generate");
        let resp = Resp {
            body: "code".into(),
        };
        cache.put(&r, &resp).unwrap();
        let got: Resp = cache.get(&r).expect("hit");
        assert_eq!(got, resp);
    }

    #[test]
    fn miss_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AiCache::new(dir.path());
        let r = req("generate");
        let got: Option<Resp> = cache.get(&r);
        assert!(got.is_none());
    }

    #[test]
    fn key_is_stable_across_hashmap_iteration_order() {
        // Build the same logical request twice, with insertions in two
        // different orders. The canonical key must match.
        let mut a = HashMap::new();
        a.insert("x".to_string(), "1".to_string());
        a.insert("y".to_string(), "2".to_string());
        let mut b = HashMap::new();
        b.insert("y".to_string(), "2".to_string());
        b.insert("x".to_string(), "1".to_string());
        let ra = Req {
            kind: "k".into(),
            params: a,
        };
        let rb = Req {
            kind: "k".into(),
            params: b,
        };
        assert_eq!(compute_key(&ra).unwrap(), compute_key(&rb).unwrap());
    }

    #[test]
    fn key_differs_for_different_input() {
        let r1 = req("generate");
        let r2 = req("repair");
        assert_ne!(compute_key(&r1).unwrap(), compute_key(&r2).unwrap());
    }

    #[test]
    fn contains_reflects_state() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AiCache::new(dir.path());
        let r = req("generate");
        assert!(!cache.contains(&r));
        cache.put(&r, &Resp { body: "x".into() }).unwrap();
        assert!(cache.contains(&r));
    }

    #[test]
    fn sharded_storage_layout() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AiCache::new(dir.path());
        let r = req("generate");
        cache.put(&r, &Resp { body: "x".into() }).unwrap();

        let key = compute_key(&r).unwrap();
        let shard_dir = cache.root().join(&key[..2]);
        assert!(shard_dir.is_dir(), "expected shard dir at {shard_dir:?}");
        let entry = shard_dir.join(format!("{key}.json"));
        assert!(entry.exists(), "expected entry file at {entry:?}");
    }

    #[test]
    fn stats_count_entries_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AiCache::new(dir.path());
        cache.put(&req("a"), &Resp { body: "one".into() }).unwrap();
        cache.put(&req("b"), &Resp { body: "two".into() }).unwrap();
        let stats = cache.stats().unwrap();
        assert_eq!(stats.entries, 2);
        assert!(stats.total_bytes > 0);
    }

    #[test]
    fn clear_removes_all_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AiCache::new(dir.path());
        cache.put(&req("a"), &Resp { body: "x".into() }).unwrap();
        cache.clear().unwrap();
        assert_eq!(cache.stats().unwrap().entries, 0);
        assert!(!cache.root().exists());
    }

    #[test]
    fn clear_on_missing_root_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let cache = AiCache::new(dir.path());
        cache.clear().expect("no-op on missing dir");
    }
}
