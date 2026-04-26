# Spec Excerpt: Package Manager

## Package Manifest (bock.package)
```toml
[package]
name = "http-framework"
version = "2.1.0"
[package.targets]
supported = ["js", "rust", "go"]
[dependencies]
core-http = "^1.0"
[dependencies.target.js]
node-adapter = "^1.0"
[dev-dependencies]
test-client = "^1.0"
[features]
default = ["json"]
```

## Resolution
Semver (^, ~, exact). PubGrub algorithm.
Target filtering, feature unification.
Transitive deps private by default.

## Lockfile (bock.lock)
Package name, version, source registry, checksum.
AI model version + hash. Decision pins (production).

## Registries
Open HTTPS REST API. Default: bock-packages.org.
Private registries configurable. Scoped: `@company/pkg`.

## Workspaces
```toml
[workspace]
members = ["packages/core", "packages/web"]
[workspace.dependencies]
shared-dep = "^1.0"
```

## Stability Tiers
stable, beta, experimental. Production can reject below threshold.
