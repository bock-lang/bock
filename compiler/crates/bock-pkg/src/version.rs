//! Version parsing utilities for Bock package constraints.
//!
//! Converts Bock version requirement strings (e.g., `^1.0`, `~2.3.1`, `=1.0.0`)
//! into `semver::VersionReq` and provides conversion to pubgrub `Ranges`.

use pubgrub::Ranges;
use semver::{Version, VersionReq};

use crate::error::PkgError;

/// Parse a Bock version requirement string into a `semver::VersionReq`.
///
/// Supports `^`, `~`, `=`, `>=`, `<=`, `>`, `<` prefixes and bare versions.
/// A bare version like `"1.0.0"` is treated as `"^1.0.0"`.
pub fn parse_version_req(s: &str) -> Result<VersionReq, PkgError> {
    let s = s.trim();
    // Bare version number without operator → treat as caret
    let req_str = if s.starts_with(|c: char| c.is_ascii_digit()) {
        format!("^{s}")
    } else {
        s.to_string()
    };
    VersionReq::parse(&req_str).map_err(|e| PkgError::InvalidVersion(format!("{s}: {e}")))
}

/// Parse a version string into a `semver::Version`.
pub fn parse_version(s: &str) -> Result<Version, PkgError> {
    // Allow two-component versions like "1.0" by appending ".0"
    let normalized = if s.matches('.').count() < 2 {
        format!("{s}.0")
    } else {
        s.to_string()
    };
    Version::parse(&normalized).map_err(|e| PkgError::InvalidVersion(format!("{s}: {e}")))
}

/// Convert a `semver::VersionReq` into a pubgrub `Ranges<Version>`.
///
/// This maps semver comparators to pubgrub range operations.
#[must_use]
pub fn req_to_pubgrub_range(req: &VersionReq) -> Ranges<Version> {
    // Build ranges from each comparator and intersect them
    let mut result = Ranges::full();

    for comp in &req.comparators {
        let range = comparator_to_range(comp);
        result = result.intersection(&range);
    }

    result
}

fn comparator_to_range(comp: &semver::Comparator) -> Ranges<Version> {
    let major = comp.major;
    let minor = comp.minor.unwrap_or(0);
    let patch = comp.patch.unwrap_or(0);
    let version = Version::new(major, minor, patch);

    match comp.op {
        semver::Op::Exact => Ranges::singleton(version),
        semver::Op::Greater => Ranges::strictly_higher_than(version),
        semver::Op::GreaterEq => Ranges::higher_than(version),
        semver::Op::Less => Ranges::strictly_lower_than(version),
        semver::Op::LessEq => {
            // <= v means < next version
            let next = Version::new(major, minor, patch + 1);
            Ranges::strictly_lower_than(next)
        }
        semver::Op::Tilde => {
            // ~X.Y.Z: >=X.Y.Z, <X.(Y+1).0
            let upper = Version::new(major, minor + 1, 0);
            Ranges::between(version, upper)
        }
        semver::Op::Caret => {
            // ^X.Y.Z: >=X.Y.Z, <next breaking
            let upper = if major > 0 {
                Version::new(major + 1, 0, 0)
            } else if minor > 0 {
                Version::new(0, minor + 1, 0)
            } else {
                Version::new(0, 0, patch + 1)
            };
            Ranges::between(version, upper)
        }
        semver::Op::Wildcard => {
            // X.Y.* or X.*
            if comp.minor.is_some() {
                // X.Y.*: >=X.Y.0, <X.(Y+1).0
                let lower = Version::new(major, minor, 0);
                let upper = Version::new(major, minor + 1, 0);
                Ranges::between(lower, upper)
            } else {
                // X.*: >=X.0.0, <(X+1).0.0
                let lower = Version::new(major, 0, 0);
                let upper = Version::new(major + 1, 0, 0);
                Ranges::between(lower, upper)
            }
        }
        _ => Ranges::full(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_caret_requirement() {
        let req = parse_version_req("^1.0").unwrap();
        assert!(req.matches(&Version::new(1, 5, 0)));
        assert!(!req.matches(&Version::new(2, 0, 0)));
    }

    #[test]
    fn parse_tilde_requirement() {
        let req = parse_version_req("~1.2.3").unwrap();
        assert!(req.matches(&Version::new(1, 2, 5)));
        assert!(!req.matches(&Version::new(1, 3, 0)));
    }

    #[test]
    fn parse_bare_version_as_caret() {
        let req = parse_version_req("1.0.0").unwrap();
        assert!(req.matches(&Version::new(1, 5, 0)));
        assert!(!req.matches(&Version::new(2, 0, 0)));
    }

    #[test]
    fn parse_exact_requirement() {
        let req = parse_version_req("=1.2.3").unwrap();
        assert!(req.matches(&Version::new(1, 2, 3)));
        assert!(!req.matches(&Version::new(1, 2, 4)));
    }

    #[test]
    fn caret_to_pubgrub_range() {
        let req = parse_version_req("^1.0").unwrap();
        let range = req_to_pubgrub_range(&req);
        assert!(range.contains(&Version::new(1, 0, 0)));
        assert!(range.contains(&Version::new(1, 9, 9)));
        assert!(!range.contains(&Version::new(2, 0, 0)));
        assert!(!range.contains(&Version::new(0, 9, 9)));
    }

    #[test]
    fn tilde_to_pubgrub_range() {
        let req = parse_version_req("~1.2.0").unwrap();
        let range = req_to_pubgrub_range(&req);
        assert!(range.contains(&Version::new(1, 2, 0)));
        assert!(range.contains(&Version::new(1, 2, 9)));
        assert!(!range.contains(&Version::new(1, 3, 0)));
    }

    #[test]
    fn parse_two_component_version() {
        let v = parse_version("1.0").unwrap();
        assert_eq!(v, Version::new(1, 0, 0));
    }
}
