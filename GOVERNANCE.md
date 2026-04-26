# Governance

Bock is pre-1.0. Governance is intentionally minimal at this stage
and will formalize as the contributor base grows.

## Decision-Making

- **Day-to-day decisions** (bug fixes, small features, refactors)
  are made by the contributor opening the PR, with reviewer signoff.
- **Language-level decisions** (grammar, semantics, type system,
  effect system, public CLI surface) require an RFC and maintainer
  consensus before implementation.
- **Breaking changes pre-1.0** require maintainer signoff and a
  CHANGELOG entry. Post-1.0, breaking changes follow semver and a
  deprecation period.

## Maintainers

The current maintainer set is recorded in `.github/CODEOWNERS` once
that file is established. Until then, the repository owner is the
sole maintainer.

## Adding Maintainers

A contributor becomes a maintainer by sustained contribution and
existing-maintainer consensus. There is no fixed criterion at this
stage.

## Conflict Resolution

Disagreements that cannot be resolved in PR review escalate to a
maintainer discussion in the relevant issue or a dedicated thread.
The maintainer set has the final say pre-1.0.

## Future

Once the project has multiple maintainers and external contributors,
this document will be replaced with a more formal governance model
(likely a steering committee with rotating membership and a public
RFC process).
