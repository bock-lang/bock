//! High-level package installation: fetch from registry, extract, lock.
//!
//! The install flow glues the pieces together:
//!
//! 1. Resolve a version for `name` (from `version_req`, or the registry's
//!    `latest` if none is given).
//! 2. Download the tarball via [`NetworkRegistry`], which caches it under
//!    the cache directory and verifies its SHA-256 checksum.
//! 3. Extract the tarball into `.bock/packages/<name>/<version>/`.
//! 4. Update `bock.package` to list the dependency.
//! 5. Update `bock.lock` with a [`LockedPackage`] entry carrying the exact
//!    resolved version and checksum for reproducibility.
//!
//! Offline mode: when no registry can be reached and a matching tarball
//! already sits in the cache, installation is still possible — the tarball
//! is extracted and the lockfile kept consistent, but the manifest entry
//! records whichever version was already cached.

use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use semver::{Version, VersionReq};
use tar::Archive;

use crate::commands;
use crate::error::{PkgError, PkgResult};
use crate::lockfile::{LockedPackage, Lockfile};
use crate::manifest::{DependencySpec, Manifest};
use crate::network::{normalize_checksum, FetchedPackage, NetworkRegistry};
use crate::version::parse_version_req;

/// Relative path (under a project) where extracted packages are installed.
pub const PACKAGES_SUBDIR: &str = ".bock/packages";

/// Relative path (under a project) of the tarball cache.
pub const CACHE_SUBDIR: &str = ".bock/cache";

/// Options controlling a single package install.
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Do not hit the network — use only cached tarballs. Errors if the
    /// requested version is not already present in the cache.
    pub offline: bool,
    /// Version requirement (e.g. `"^1.0"`). `None` → install the registry's
    /// latest version.
    pub version_req: Option<String>,
}

/// Information about a newly installed package.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    /// Package name as listed in the manifest.
    pub name: String,
    /// Exact resolved version (semver).
    pub version: Version,
    /// Where the package was extracted (absolute path).
    pub install_dir: PathBuf,
    /// SHA-256 hex of the tarball, written to the lockfile.
    pub checksum: String,
    /// Source URL the package was fetched from (registry base), or `"cache"`
    /// when the install was served entirely from the local cache.
    pub source: String,
}

/// Install a package: download, extract, and update the manifest + lockfile.
///
/// `project_dir` is the directory containing `bock.package` (and where the
/// `.bock/` subtree will live). `registry` must already be configured with
/// the caller's cache directory and, optionally, an auth token.
pub fn install_package(
    project_dir: &Path,
    registry: &NetworkRegistry,
    name: &str,
    options: &InstallOptions,
) -> PkgResult<InstalledPackage> {
    let manifest_path = project_dir.join(commands::MANIFEST_FILE);
    if !manifest_path.exists() {
        return Err(PkgError::Io(format!(
            "no {} found in {}",
            commands::MANIFEST_FILE,
            project_dir.display()
        )));
    }

    let resolved = resolve_and_fetch(registry, name, options)?;

    let install_dir = project_dir
        .join(PACKAGES_SUBDIR)
        .join(name)
        .join(resolved.version.to_string());
    extract_tarball(&resolved.tarball_path, &install_dir)?;

    let version_spec = options
        .version_req
        .clone()
        .unwrap_or_else(|| format!("^{}", resolved.version));
    commands::add(&manifest_path, name, Some(&version_spec))?;

    let lock_path = project_dir.join(commands::LOCKFILE);
    let lockfile = update_lockfile(
        &lock_path,
        &manifest_path,
        name,
        &resolved.version,
        &resolved.checksum,
        &resolved.source,
    )?;
    lockfile.write_to_file(&lock_path)?;

    Ok(InstalledPackage {
        name: name.to_string(),
        version: resolved.version,
        install_dir,
        checksum: resolved.checksum,
        source: resolved.source,
    })
}

/// Wipe every tarball out of the cache directory.
///
/// The directory itself is kept so subsequent operations can repopulate it.
/// Returns the number of files removed.
pub fn clear_cache(cache_dir: &Path) -> PkgResult<usize> {
    if !cache_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(cache_dir).map_err(|e| PkgError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| PkgError::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| PkgError::Io(e.to_string()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Extract a `.tar.gz` archive into `target_dir`, creating parents as needed.
///
/// Existing contents of `target_dir` are wiped first so a re-install always
/// starts from a clean state.
pub fn extract_tarball(tarball_path: &Path, target_dir: &Path) -> PkgResult<()> {
    if target_dir.exists() {
        std::fs::remove_dir_all(target_dir).map_err(|e| PkgError::Io(e.to_string()))?;
    }
    std::fs::create_dir_all(target_dir).map_err(|e| PkgError::Io(e.to_string()))?;

    let file = std::fs::File::open(tarball_path).map_err(|e| PkgError::Io(e.to_string()))?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(target_dir)
        .map_err(|e| PkgError::Io(format!("extracting {}: {}", tarball_path.display(), e)))?;
    Ok(())
}

/// Resolution outcome shared by online and offline paths.
struct Resolved {
    version: Version,
    tarball_path: PathBuf,
    checksum: String,
    source: String,
}

fn resolve_and_fetch(
    registry: &NetworkRegistry,
    name: &str,
    options: &InstallOptions,
) -> PkgResult<Resolved> {
    let req = match &options.version_req {
        Some(s) => Some(parse_version_req(s)?),
        None => None,
    };

    if options.offline {
        let (version, tarball_path, checksum) =
            resolve_from_cache(registry.cache_dir(), name, req.as_ref())?;
        return Ok(Resolved {
            version,
            tarball_path,
            checksum,
            source: "cache".to_string(),
        });
    }

    // Online path: ask the registry, fall back to cache on transport failures.
    match fetch_from_network(registry, name, req.as_ref()) {
        Ok((version, fetched)) => Ok(Resolved {
            version,
            tarball_path: fetched.tarball_path,
            checksum: fetched.checksum,
            source: registry.base_url().to_string(),
        }),
        Err(PkgError::Network(msg)) => {
            // Offline fallback: see if the cache can satisfy the request.
            if let Ok((version, tarball_path, checksum)) =
                resolve_from_cache(registry.cache_dir(), name, req.as_ref())
            {
                return Ok(Resolved {
                    version,
                    tarball_path,
                    checksum,
                    source: "cache".to_string(),
                });
            }
            Err(PkgError::Network(format!(
                "{msg}\n\nhint: pass --offline to use a cached tarball, or check your network connection"
            )))
        }
        Err(e) => Err(e),
    }
}

fn fetch_from_network(
    registry: &NetworkRegistry,
    name: &str,
    req: Option<&VersionReq>,
) -> PkgResult<(Version, FetchedPackage)> {
    let versions = registry.fetch_versions(name)?;
    let version = pick_version(&versions.versions, req)?.unwrap_or_else(|| versions.latest.clone());
    let fetched = registry.fetch_package(name, &version)?;
    let parsed = crate::version::parse_version(&version)?;
    Ok((parsed, fetched))
}

fn resolve_from_cache(
    cache_dir: &Path,
    name: &str,
    req: Option<&VersionReq>,
) -> PkgResult<(Version, PathBuf, String)> {
    let prefix = format!("{name}-");
    let suffix = ".tar.gz";
    let mut candidates: Vec<(Version, PathBuf)> = Vec::new();

    if cache_dir.exists() {
        for entry in std::fs::read_dir(cache_dir).map_err(|e| PkgError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| PkgError::Io(e.to_string()))?;
            let Some(fname) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !fname.starts_with(&prefix) || !fname.ends_with(suffix) {
                continue;
            }
            let ver_str = &fname[prefix.len()..fname.len() - suffix.len()];
            if let Ok(ver) = crate::version::parse_version(ver_str) {
                if req.is_none_or(|r| r.matches(&ver)) {
                    candidates.push((ver, entry.path()));
                }
            }
        }
    }

    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    let Some((version, path)) = candidates.into_iter().next() else {
        return Err(PkgError::PackageNotFound(format!(
            "no cached tarball for '{name}' matches the requested version (cache: {})",
            cache_dir.display()
        )));
    };

    let bytes = std::fs::read(&path).map_err(|e| PkgError::Io(e.to_string()))?;
    let checksum = crate::network::sha256_hex(&bytes);
    Ok((version, path, checksum))
}

/// Pick the highest version from `versions` satisfying `req` (or `None` when
/// no requirement is given — the caller falls back to the registry's `latest`).
fn pick_version(versions: &[String], req: Option<&VersionReq>) -> PkgResult<Option<String>> {
    let Some(req) = req else {
        return Ok(None);
    };
    let mut best: Option<Version> = None;
    for v in versions {
        let parsed = match crate::version::parse_version(v) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if req.matches(&parsed) && best.as_ref().is_none_or(|b| parsed > *b) {
            best = Some(parsed);
        }
    }
    match best {
        Some(v) => Ok(Some(v.to_string())),
        None => Err(PkgError::ResolutionFailed(format!(
            "no version of this package matches `{req}`"
        ))),
    }
}

fn update_lockfile(
    lock_path: &Path,
    manifest_path: &Path,
    new_name: &str,
    new_version: &Version,
    new_checksum: &str,
    new_source: &str,
) -> PkgResult<Lockfile> {
    let manifest = Manifest::from_file(manifest_path)?;
    let mut lockfile = if lock_path.exists() {
        Lockfile::from_file(lock_path)?
    } else {
        Lockfile {
            version: 1,
            packages: Vec::new(),
        }
    };

    // Drop any stale entry for this package so we can replace it.
    lockfile.packages.retain(|p| p.name != new_name);
    lockfile.packages.push(LockedPackage {
        name: new_name.to_string(),
        version: new_version.to_string(),
        source: Some(new_source.to_string()),
        checksum: Some(format!("sha256:{}", normalize_checksum(new_checksum))),
        dependencies: manifest
            .dependencies
            .common
            .iter()
            .filter_map(|(dep_name, spec)| {
                if dep_name == new_name {
                    return None;
                }
                match spec {
                    DependencySpec::Simple(v) => Some((dep_name.clone(), v.clone())),
                    DependencySpec::Detailed(d) => {
                        d.version.as_ref().map(|v| (dep_name.clone(), v.clone()))
                    }
                }
            })
            .collect(),
    });
    lockfile.packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(lockfile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use mockito::Server;

    fn make_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let encoder = GzEncoder::new(&mut out, Compression::default());
            let mut builder = tar::Builder::new(encoder);
            for (path, contents) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(contents.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, path, *contents).unwrap();
            }
            let encoder = builder.into_inner().unwrap();
            encoder.finish().unwrap();
        }
        out
    }

    fn write_manifest(dir: &Path, name: &str) {
        std::fs::write(
            dir.join(commands::MANIFEST_FILE),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
    }

    fn ensure_empty_token_env() {
        // Tests share process state; clear any token from the host shell so
        // requests aren't unexpectedly Bearer-authed during mockito assertions.
        std::env::remove_var(crate::network::AUTH_TOKEN_ENV);
    }

    #[test]
    fn extract_tarball_places_files() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_path = tmp.path().join("pkg.tar.gz");
        let bytes = make_tarball(&[("src/main.bock", b"module main"), ("README.md", b"hi")]);
        std::fs::write(&tar_path, &bytes).unwrap();

        let target = tmp.path().join("out");
        extract_tarball(&tar_path, &target).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.join("src/main.bock")).unwrap(),
            "module main"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("README.md")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn install_package_writes_manifest_lockfile_and_unpacks() {
        ensure_empty_token_env();
        let project = tempfile::tempdir().unwrap();
        write_manifest(project.path(), "my-app");

        let tarball_bytes = make_tarball(&[("src/lib.bock", b"module foo\n")]);
        let checksum = crate::network::sha256_hex(&tarball_bytes);

        let mut server = Server::new();
        let _versions = server
            .mock("GET", "/packages/foo")
            .with_status(200)
            .with_body(r#"{"versions":["1.0.0","1.2.0"],"latest":"1.2.0"}"#)
            .create();
        let meta_body = format!(
            r#"{{"manifest":{{"dependencies":{{}}}},"checksum":"sha256:{checksum}","download_url":""}}"#
        );
        let _meta = server
            .mock("GET", "/packages/foo/1.2.0")
            .with_status(200)
            .with_body(meta_body)
            .create();
        let _download = server
            .mock("GET", "/packages/foo/1.2.0/download")
            .with_status(200)
            .with_body(tarball_bytes.clone())
            .create();

        let cache_dir = project.path().join(CACHE_SUBDIR);
        let registry = NetworkRegistry::new(server.url(), &cache_dir).unwrap();

        let installed =
            install_package(project.path(), &registry, "foo", &InstallOptions::default()).unwrap();

        assert_eq!(installed.version.to_string(), "1.2.0");
        assert!(installed.install_dir.ends_with(".bock/packages/foo/1.2.0"));
        assert_eq!(
            std::fs::read_to_string(installed.install_dir.join("src/lib.bock")).unwrap(),
            "module foo\n"
        );

        // Manifest updated.
        let manifest = Manifest::from_file(&project.path().join(commands::MANIFEST_FILE)).unwrap();
        assert_eq!(manifest.dependencies["foo"].version_req(), Some("^1.2.0"));

        // Lockfile written with checksum and source.
        let lockfile = Lockfile::from_file(&project.path().join(commands::LOCKFILE)).unwrap();
        let entry = lockfile
            .packages
            .iter()
            .find(|p| p.name == "foo")
            .expect("lockfile entry missing");
        assert_eq!(entry.version, "1.2.0");
        assert_eq!(
            entry.checksum.as_deref(),
            Some(format!("sha256:{checksum}").as_str())
        );
        assert_eq!(entry.source.as_deref(), Some(server.url().as_str()));
    }

    #[test]
    fn install_respects_version_requirement() {
        ensure_empty_token_env();
        let project = tempfile::tempdir().unwrap();
        write_manifest(project.path(), "my-app");

        let tarball_bytes = make_tarball(&[("src/lib.bock", b"")]);
        let checksum = crate::network::sha256_hex(&tarball_bytes);

        let mut server = Server::new();
        let _versions = server
            .mock("GET", "/packages/foo")
            .with_status(200)
            .with_body(r#"{"versions":["1.0.0","1.5.0","2.0.0"],"latest":"2.0.0"}"#)
            .create();
        let meta_body = format!(
            r#"{{"manifest":{{"dependencies":{{}}}},"checksum":"sha256:{checksum}","download_url":""}}"#
        );
        let _meta = server
            .mock("GET", "/packages/foo/1.5.0")
            .with_status(200)
            .with_body(meta_body)
            .create();
        let _download = server
            .mock("GET", "/packages/foo/1.5.0/download")
            .with_status(200)
            .with_body(tarball_bytes)
            .create();

        let cache_dir = project.path().join(CACHE_SUBDIR);
        let registry = NetworkRegistry::new(server.url(), &cache_dir).unwrap();

        let options = InstallOptions {
            version_req: Some("^1.0".to_string()),
            offline: false,
        };
        let installed = install_package(project.path(), &registry, "foo", &options).unwrap();

        assert_eq!(installed.version.to_string(), "1.5.0");
    }

    #[test]
    fn install_uses_cache_in_offline_mode() {
        ensure_empty_token_env();
        let project = tempfile::tempdir().unwrap();
        write_manifest(project.path(), "my-app");

        let cache_dir = project.path().join(CACHE_SUBDIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let tarball_bytes = make_tarball(&[("README", b"offline")]);
        let cached = cache_dir.join("foo-1.4.0.tar.gz");
        std::fs::write(&cached, &tarball_bytes).unwrap();
        let checksum = crate::network::sha256_hex(&tarball_bytes);

        // Point at a dead URL so any network access would fail.
        let registry = NetworkRegistry::new("http://127.0.0.1:1/", &cache_dir).unwrap();
        let options = InstallOptions {
            offline: true,
            version_req: None,
        };
        let installed = install_package(project.path(), &registry, "foo", &options).unwrap();

        assert_eq!(installed.version.to_string(), "1.4.0");
        assert_eq!(installed.checksum, checksum);
        assert_eq!(installed.source, "cache");
    }

    #[test]
    fn install_offline_errors_when_not_cached() {
        ensure_empty_token_env();
        let project = tempfile::tempdir().unwrap();
        write_manifest(project.path(), "my-app");
        let cache_dir = project.path().join(CACHE_SUBDIR);

        let registry = NetworkRegistry::new("http://127.0.0.1:1/", &cache_dir).unwrap();
        let err = install_package(
            project.path(),
            &registry,
            "foo",
            &InstallOptions {
                offline: true,
                version_req: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, PkgError::PackageNotFound(_)));
    }

    #[test]
    fn clear_cache_removes_tarballs() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("a-1.0.0.tar.gz"), b"x").unwrap();
        std::fs::write(cache.join("b-2.0.0.tar.gz"), b"y").unwrap();

        let removed = clear_cache(&cache).unwrap();
        assert_eq!(removed, 2);
        assert!(cache.exists());
        assert!(std::fs::read_dir(&cache).unwrap().next().is_none());
    }

    #[test]
    fn pick_version_selects_highest_matching() {
        let versions = vec![
            "0.9.0".into(),
            "1.0.0".into(),
            "1.2.3".into(),
            "2.0.0".into(),
        ];
        let req = VersionReq::parse("^1.0").unwrap();
        let picked = pick_version(&versions, Some(&req)).unwrap();
        assert_eq!(picked.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn pick_version_errors_when_nothing_matches() {
        let versions = vec!["0.1.0".into(), "0.2.0".into()];
        let req = VersionReq::parse("^1.0").unwrap();
        let err = pick_version(&versions, Some(&req)).unwrap_err();
        assert!(matches!(err, PkgError::ResolutionFailed(_)));
    }

    #[test]
    fn install_sends_bearer_auth_when_configured() {
        ensure_empty_token_env();
        let project = tempfile::tempdir().unwrap();
        write_manifest(project.path(), "my-app");

        let tarball_bytes = make_tarball(&[("README", b"tok")]);
        let checksum = crate::network::sha256_hex(&tarball_bytes);

        let mut server = Server::new();
        let _v = server
            .mock("GET", "/packages/foo")
            .match_header("authorization", "Bearer secret-xyz")
            .with_status(200)
            .with_body(r#"{"versions":["1.0.0"],"latest":"1.0.0"}"#)
            .create();
        let meta_body = format!(
            r#"{{"manifest":{{"dependencies":{{}}}},"checksum":"sha256:{checksum}","download_url":""}}"#
        );
        let _m = server
            .mock("GET", "/packages/foo/1.0.0")
            .match_header("authorization", "Bearer secret-xyz")
            .with_status(200)
            .with_body(meta_body)
            .create();
        let _d = server
            .mock("GET", "/packages/foo/1.0.0/download")
            .match_header("authorization", "Bearer secret-xyz")
            .with_status(200)
            .with_body(tarball_bytes)
            .create();

        let cache_dir = project.path().join(CACHE_SUBDIR);
        let registry = NetworkRegistry::new(server.url(), &cache_dir)
            .unwrap()
            .with_auth_token(Some("secret-xyz".to_string()));
        install_package(project.path(), &registry, "foo", &InstallOptions::default()).unwrap();
    }
}
