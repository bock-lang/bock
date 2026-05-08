//! Network-backed package registry client.
//!
//! Implements the registry protocol defined in spec §19.5:
//!
//! ```text
//! GET /packages/{name}                   → { versions, latest }
//! GET /packages/{name}/{version}         → { manifest, checksum, download_url }
//! GET /packages/{name}/{version}/download → tarball bytes
//! ```
//!
//! Tarballs are cached under `cache_dir` after SHA-256 verification.
//! A [`PackageRegistry`] can be passed as a fallback for offline use
//! or private overrides.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::PkgError;
use crate::resolver::{PackageRegistry, PackageVersionMeta};

/// Response body for `GET /packages/{name}`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct VersionsResponse {
    /// All versions known for this package (semver strings).
    pub versions: Vec<String>,
    /// The latest (highest) stable version, hinted by the registry.
    pub latest: String,
}

/// Response body for `GET /packages/{name}/{version}`.
#[derive(Debug, Clone, Deserialize)]
pub struct VersionMetaResponse {
    /// Subset of the package manifest relevant for resolution.
    pub manifest: ManifestData,
    /// SHA-256 digest of the tarball, optionally prefixed with `"sha256:"`.
    pub checksum: String,
    /// URL from which the tarball can be fetched. If empty, the default
    /// `/packages/{name}/{version}/download` endpoint is used.
    #[serde(default)]
    pub download_url: String,
}

/// Manifest fragment served by the registry for a specific version.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ManifestData {
    /// Direct dependencies: name → version requirement string.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    /// Targets this version supports. `None` = all targets.
    #[serde(default)]
    pub supported_targets: Option<Vec<String>>,
    /// Features declared by this version.
    #[serde(default)]
    pub available_features: BTreeMap<String, Vec<String>>,
    /// Features requested from each dependency.
    #[serde(default)]
    pub dep_features: BTreeMap<String, Vec<String>>,
}

/// Environment variable that supplies a Bearer auth token for private registries.
pub const AUTH_TOKEN_ENV: &str = "BOCK_REGISTRY_TOKEN";

/// A network-backed registry that fetches metadata and tarballs over HTTPS.
///
/// Hydrate into a [`PackageRegistry`] via [`Self::hydrate`] before running
/// resolution, since resolution is driven by the in-memory provider.
pub struct NetworkRegistry {
    base_url: String,
    client: reqwest::blocking::Client,
    cache_dir: PathBuf,
    fallback: Option<PackageRegistry>,
    auth_token: Option<String>,
}

impl NetworkRegistry {
    /// Build a client pointed at `base_url` with tarballs cached under `cache_dir`.
    ///
    /// The cache directory is created if it does not exist.
    pub fn new(
        base_url: impl Into<String>,
        cache_dir: impl Into<PathBuf>,
    ) -> Result<Self, PkgError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("bock-pkg/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| PkgError::Network(e.to_string()))?;
        let cache_dir = cache_dir.into();
        std::fs::create_dir_all(&cache_dir).map_err(|e| PkgError::Io(e.to_string()))?;
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Ok(Self {
            base_url,
            client,
            cache_dir,
            fallback: None,
            auth_token: std::env::var(AUTH_TOKEN_ENV).ok().filter(|s| !s.is_empty()),
        })
    }

    /// Attach an in-memory [`PackageRegistry`] to serve entries the network
    /// does not know about (or cannot be reached for).
    #[must_use]
    pub fn with_fallback(mut self, fallback: PackageRegistry) -> Self {
        self.fallback = Some(fallback);
        self
    }

    /// Override the Bearer auth token used for registry requests.
    ///
    /// Pass `None` to clear the token (useful for tests that want to skip the
    /// environment-provided value). By default, the value of the
    /// [`AUTH_TOKEN_ENV`] environment variable is used.
    #[must_use]
    pub fn with_auth_token(mut self, token: Option<String>) -> Self {
        self.auth_token = token.filter(|s| !s.is_empty());
        self
    }

    /// The Bearer auth token currently in effect, if any.
    #[must_use]
    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    fn authed_get(&self, url: &str) -> reqwest::blocking::RequestBuilder {
        let mut req = self.client.get(url);
        if let Some(token) = &self.auth_token {
            req = req.bearer_auth(token);
        }
        req
    }

    /// Base URL of the registry (with any trailing slash stripped).
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Directory where downloaded tarballs are cached.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Fetch the list of versions available for `name`.
    pub fn fetch_versions(&self, name: &str) -> Result<VersionsResponse, PkgError> {
        let url = format!("{}/packages/{}", self.base_url, name);
        let response = self
            .authed_get(&url)
            .send()
            .map_err(|e| PkgError::Network(format!("GET {url}: {e}")))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PkgError::PackageNotFound(name.to_string()));
        }
        if !response.status().is_success() {
            return Err(PkgError::Network(format!(
                "GET {url}: status {}",
                response.status()
            )));
        }
        response
            .json()
            .map_err(|e| PkgError::Network(format!("decoding {url}: {e}")))
    }

    /// Fetch the metadata for a specific `version` of `name`.
    pub fn fetch_version_meta(
        &self,
        name: &str,
        version: &str,
    ) -> Result<VersionMetaResponse, PkgError> {
        let url = format!("{}/packages/{}/{}", self.base_url, name, version);
        let response = self
            .authed_get(&url)
            .send()
            .map_err(|e| PkgError::Network(format!("GET {url}: {e}")))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(PkgError::PackageNotFound(format!("{name}@{version}")));
        }
        if !response.status().is_success() {
            return Err(PkgError::Network(format!(
                "GET {url}: status {}",
                response.status()
            )));
        }
        response
            .json()
            .map_err(|e| PkgError::Network(format!("decoding {url}: {e}")))
    }

    /// Download a package tarball, verifying its checksum, and cache it.
    ///
    /// Returns the path of the cached tarball. If the file already exists in
    /// the cache, it is returned without re-fetching or re-verifying — the
    /// tarball name embeds the version, so the cache key is version-scoped.
    pub fn download_package(&self, name: &str, version: &str) -> Result<PathBuf, PkgError> {
        let cache_path = self.cache_dir.join(format!("{name}-{version}.tar.gz"));
        if cache_path.exists() {
            return Ok(cache_path);
        }

        let meta = self.fetch_version_meta(name, version)?;
        let tarball_url = if meta.download_url.is_empty() {
            format!("{}/packages/{}/{}/download", self.base_url, name, version)
        } else {
            meta.download_url.clone()
        };

        let response = self
            .authed_get(&tarball_url)
            .send()
            .map_err(|e| PkgError::Network(format!("GET {tarball_url}: {e}")))?;
        if !response.status().is_success() {
            return Err(PkgError::Network(format!(
                "GET {tarball_url}: status {}",
                response.status()
            )));
        }
        let bytes = response
            .bytes()
            .map_err(|e| PkgError::Network(format!("reading {tarball_url}: {e}")))?;

        verify_checksum(&bytes, &meta.checksum)?;

        std::fs::write(&cache_path, &bytes).map_err(|e| PkgError::Io(e.to_string()))?;
        Ok(cache_path)
    }

    /// Fetch a package: resolve metadata, download (or reuse cached) tarball,
    /// and return the cached path together with the metadata response.
    ///
    /// When the tarball is already cached, the checksum in the returned meta
    /// is still fetched from the registry to keep lockfile entries accurate.
    pub fn fetch_package(&self, name: &str, version: &str) -> Result<FetchedPackage, PkgError> {
        let meta = self.fetch_version_meta(name, version)?;
        let cache_path = self.cache_dir.join(format!("{name}-{version}.tar.gz"));

        if !cache_path.exists() {
            let tarball_url = if meta.download_url.is_empty() {
                format!("{}/packages/{}/{}/download", self.base_url, name, version)
            } else {
                meta.download_url.clone()
            };

            let response = self
                .authed_get(&tarball_url)
                .send()
                .map_err(|e| PkgError::Network(format!("GET {tarball_url}: {e}")))?;
            if !response.status().is_success() {
                return Err(PkgError::Network(format!(
                    "GET {tarball_url}: status {}",
                    response.status()
                )));
            }
            let bytes = response
                .bytes()
                .map_err(|e| PkgError::Network(format!("reading {tarball_url}: {e}")))?;

            verify_checksum(&bytes, &meta.checksum)?;

            std::fs::write(&cache_path, &bytes).map_err(|e| PkgError::Io(e.to_string()))?;
        } else {
            // Cache hit — sanity-check the cached bytes against the registry checksum.
            let bytes = std::fs::read(&cache_path).map_err(|e| PkgError::Io(e.to_string()))?;
            verify_checksum(&bytes, &meta.checksum)?;
        }

        Ok(FetchedPackage {
            tarball_path: cache_path,
            checksum: normalize_checksum(&meta.checksum),
            meta,
        })
    }

    /// Hydrate an in-memory [`PackageRegistry`] by fetching metadata for each
    /// of the named packages.
    ///
    /// If a `fallback` was attached, it seeds the returned registry and absorbs
    /// any packages the network layer could not resolve (offline or not found).
    /// Per-package network errors are swallowed when a fallback is present so
    /// offline operation can continue; otherwise they propagate.
    pub fn hydrate(&self, names: &[&str]) -> Result<PackageRegistry, PkgError> {
        let mut registry = self.fallback.clone().unwrap_or_default();
        for name in names {
            match self.fetch_versions(name) {
                Ok(versions) => {
                    for version in &versions.versions {
                        match self.fetch_version_meta(name, version) {
                            Ok(meta) => {
                                let pkg_meta = PackageVersionMeta {
                                    deps: meta.manifest.dependencies,
                                    dep_features: meta.manifest.dep_features,
                                    supported_targets: meta.manifest.supported_targets,
                                    available_features: meta.manifest.available_features,
                                };
                                registry.register_with_meta(name, version, pkg_meta)?;
                            }
                            Err(PkgError::Network(_)) if self.fallback.is_some() => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                Err(PkgError::Network(_)) if self.fallback.is_some() => {}
                Err(PkgError::PackageNotFound(_)) if registry.has_package(name) => {
                    // Fallback already provides it — continue.
                }
                Err(e) => return Err(e),
            }
        }
        Ok(registry)
    }
}

/// Result of [`NetworkRegistry::fetch_package`].
#[derive(Debug, Clone)]
pub struct FetchedPackage {
    /// Path to the verified tarball in the cache directory.
    pub tarball_path: PathBuf,
    /// Canonicalized checksum (bare hex, lowercased) recorded for the lockfile.
    pub checksum: String,
    /// Full metadata response from the registry.
    pub meta: VersionMetaResponse,
}

/// Strip any `sha256:` prefix and lowercase the remaining hex for storage.
#[must_use]
pub fn normalize_checksum(checksum: &str) -> String {
    checksum
        .strip_prefix("sha256:")
        .unwrap_or(checksum)
        .to_ascii_lowercase()
}

/// Verify a byte buffer against a SHA-256 checksum.
///
/// Accepts both bare hex (`"a1b2..."`) and the `"sha256:"`-prefixed form used
/// by the registry. Returns [`PkgError::ChecksumMismatch`] on disagreement.
pub fn verify_checksum(data: &[u8], expected: &str) -> Result<(), PkgError> {
    let expected_hex = expected.strip_prefix("sha256:").unwrap_or(expected);
    let actual = sha256_hex(data);
    if !actual.eq_ignore_ascii_case(expected_hex) {
        return Err(PkgError::ChecksumMismatch {
            expected: expected_hex.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Compute the hex-encoded SHA-256 digest of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The `[registries]` section of an `bock.project` file.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct RegistriesSection {
    /// URL of the default registry. Falls back to the built-in public
    /// registry if unset.
    pub default: Option<String>,
    /// Named private registries (e.g. `internal = "https://..."`).
    #[serde(flatten)]
    pub named: BTreeMap<String, String>,
}

/// Parse just the `[registries]` section out of an `bock.project` TOML string.
///
/// Other sections are ignored, so this is safe to call on any project file.
pub fn parse_registries(project_toml: &str) -> Result<RegistriesSection, PkgError> {
    #[derive(Deserialize)]
    struct Wrapper {
        #[serde(default)]
        registries: RegistriesSection,
    }
    let wrapper: Wrapper = toml::from_str(project_toml)
        .map_err(|e| PkgError::ManifestParse(format!("bock.project: {e}")))?;
    Ok(wrapper.registries)
}

/// Resolve the effective default registry URL for a project.
///
/// Reads `bock.project` in `project_dir`. If the file is missing or lacks a
/// `[registries]` section, returns `None` — callers should fall back to the
/// in-memory registry in that case.
pub fn default_registry_url(project_dir: &Path) -> Option<String> {
    let path = project_dir.join("bock.project");
    let content = std::fs::read_to_string(&path).ok()?;
    parse_registries(&content).ok()?.default
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn tmp_cache() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache");
        (dir, path)
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verify_checksum_accepts_matching_hex() {
        let bytes = b"hello world";
        let hex = sha256_hex(bytes);
        verify_checksum(bytes, &hex).unwrap();
    }

    #[test]
    fn verify_checksum_accepts_sha256_prefix() {
        let bytes = b"hello world";
        let hex = sha256_hex(bytes);
        verify_checksum(bytes, &format!("sha256:{hex}")).unwrap();
    }

    #[test]
    fn verify_checksum_rejects_mismatch() {
        let result = verify_checksum(b"hello", "sha256:deadbeef");
        assert!(matches!(result, Err(PkgError::ChecksumMismatch { .. })));
    }

    #[test]
    fn fetch_versions_parses_response() {
        let mut server = Server::new();
        let mock = server
            .mock("GET", "/packages/foo")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"versions":["1.0.0","1.1.0"],"latest":"1.1.0"}"#)
            .create();

        let (_tmp, cache) = tmp_cache();
        let reg = NetworkRegistry::new(server.url(), cache).unwrap();
        let resp = reg.fetch_versions("foo").unwrap();

        assert_eq!(resp.versions, vec!["1.0.0", "1.1.0"]);
        assert_eq!(resp.latest, "1.1.0");
        mock.assert();
    }

    #[test]
    fn fetch_versions_maps_404_to_package_not_found() {
        let mut server = Server::new();
        let _mock = server
            .mock("GET", "/packages/missing")
            .with_status(404)
            .create();

        let (_tmp, cache) = tmp_cache();
        let reg = NetworkRegistry::new(server.url(), cache).unwrap();
        let err = reg.fetch_versions("missing").unwrap_err();
        assert!(matches!(err, PkgError::PackageNotFound(_)));
    }

    #[test]
    fn fetch_version_meta_parses_manifest() {
        let mut server = Server::new();
        let body = r#"{
            "manifest": {
                "dependencies": {"bar": "^1.0"},
                "supported_targets": ["js", "rust"],
                "available_features": {"json": []}
            },
            "checksum": "sha256:abc",
            "download_url": ""
        }"#;
        let mock = server
            .mock("GET", "/packages/foo/1.0.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create();

        let (_tmp, cache) = tmp_cache();
        let reg = NetworkRegistry::new(server.url(), cache).unwrap();
        let meta = reg.fetch_version_meta("foo", "1.0.0").unwrap();

        assert_eq!(meta.checksum, "sha256:abc");
        assert_eq!(meta.manifest.dependencies["bar"], "^1.0");
        assert_eq!(
            meta.manifest.supported_targets,
            Some(vec!["js".into(), "rust".into()])
        );
        mock.assert();
    }

    #[test]
    fn download_package_verifies_and_caches() {
        let mut server = Server::new();
        let tarball = b"fake tarball contents";
        let checksum = sha256_hex(tarball);
        let body = format!(
            r#"{{"manifest":{{"dependencies":{{}}}},"checksum":"sha256:{checksum}","download_url":""}}"#
        );
        let _meta = server
            .mock("GET", "/packages/foo/1.0.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create();
        let _download = server
            .mock("GET", "/packages/foo/1.0.0/download")
            .with_status(200)
            .with_body(tarball)
            .create();

        let (_tmp, cache) = tmp_cache();
        let reg = NetworkRegistry::new(server.url(), &cache).unwrap();
        let path = reg.download_package("foo", "1.0.0").unwrap();

        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), tarball);

        // Second call should be served from cache — no new mock expectations.
        let again = reg.download_package("foo", "1.0.0").unwrap();
        assert_eq!(again, path);
    }

    #[test]
    fn download_package_rejects_bad_checksum() {
        let mut server = Server::new();
        let tarball = b"bytes that do not match";
        let body =
            r#"{"manifest":{"dependencies":{}},"checksum":"sha256:deadbeef","download_url":""}"#;
        let _meta = server
            .mock("GET", "/packages/foo/1.0.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create();
        let _download = server
            .mock("GET", "/packages/foo/1.0.0/download")
            .with_status(200)
            .with_body(tarball)
            .create();

        let (_tmp, cache) = tmp_cache();
        let reg = NetworkRegistry::new(server.url(), &cache).unwrap();
        let err = reg.download_package("foo", "1.0.0").unwrap_err();
        assert!(matches!(err, PkgError::ChecksumMismatch { .. }));
        // Nothing written to cache on failure.
        assert!(!cache.join("foo-1.0.0.tar.gz").exists());
    }

    #[test]
    fn download_package_honors_custom_download_url() {
        let mut server = Server::new();
        let tarball = b"custom url payload";
        let checksum = sha256_hex(tarball);
        let custom_url = format!("{}/mirror/foo-1.0.0.tgz", server.url());
        let body = format!(
            r#"{{"manifest":{{"dependencies":{{}}}},"checksum":"sha256:{checksum}","download_url":"{custom_url}"}}"#
        );
        let _meta = server
            .mock("GET", "/packages/foo/1.0.0")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create();
        let _download = server
            .mock("GET", "/mirror/foo-1.0.0.tgz")
            .with_status(200)
            .with_body(tarball)
            .create();

        let (_tmp, cache) = tmp_cache();
        let reg = NetworkRegistry::new(server.url(), &cache).unwrap();
        let path = reg.download_package("foo", "1.0.0").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), tarball);
    }

    #[test]
    fn hydrate_populates_registry_from_network() {
        let mut server = Server::new();
        let _v = server
            .mock("GET", "/packages/foo")
            .with_status(200)
            .with_body(r#"{"versions":["1.0.0"],"latest":"1.0.0"}"#)
            .create();
        let _m = server
            .mock("GET", "/packages/foo/1.0.0")
            .with_status(200)
            .with_body(
                r#"{"manifest":{"dependencies":{"bar":"^1.0"}},"checksum":"sha256:x","download_url":""}"#,
            )
            .create();

        let (_tmp, cache) = tmp_cache();
        let reg = NetworkRegistry::new(server.url(), cache).unwrap();
        let registry = reg.hydrate(&["foo"]).unwrap();

        assert!(registry.has_package("foo"));
        assert_eq!(registry.available_versions("foo").len(), 1);
    }

    #[test]
    fn hydrate_falls_back_when_network_unreachable() {
        // Point at a URL with no server listening; with a fallback, hydration
        // should swallow the transport error and return the fallback entries.
        let mut fallback = PackageRegistry::new();
        fallback.register("foo", "1.0.0", BTreeMap::new()).unwrap();

        let (_tmp, cache) = tmp_cache();
        let reg = NetworkRegistry::new("http://127.0.0.1:1/", cache)
            .unwrap()
            .with_fallback(fallback);

        let registry = reg.hydrate(&["foo"]).unwrap();
        assert!(registry.has_package("foo"));
    }

    #[test]
    fn parse_registries_reads_default_and_named() {
        let project = r#"
[project]
name = "test"
version = "0.1.0"

[registries]
default = "https://registry.bock-lang.dev/api/v1"
internal = "https://bock.company.internal"
"#;
        let regs = parse_registries(project).unwrap();
        assert_eq!(
            regs.default.as_deref(),
            Some("https://registry.bock-lang.dev/api/v1")
        );
        assert_eq!(
            regs.named.get("internal").map(String::as_str),
            Some("https://bock.company.internal"),
        );
    }

    #[test]
    fn parse_registries_missing_section_is_empty() {
        let project = r#"
[project]
name = "test"
version = "0.1.0"
"#;
        let regs = parse_registries(project).unwrap();
        assert!(regs.default.is_none());
        assert!(regs.named.is_empty());
    }

    #[test]
    fn default_registry_url_reads_from_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("bock.project"),
            "[project]\nname = \"t\"\nversion = \"0.1.0\"\n\n[registries]\ndefault = \"https://example.com/api/v1\"\n",
        )
        .unwrap();
        assert_eq!(
            default_registry_url(dir.path()).as_deref(),
            Some("https://example.com/api/v1")
        );
    }

    #[test]
    fn default_registry_url_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(default_registry_url(dir.path()).is_none());
    }
}
