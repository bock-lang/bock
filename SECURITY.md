# Security Policy

## Supported Versions

Bock is pre-1.0. Only the latest released minor version receives
fixes. Pin to a specific version in production at your own risk.

| Version | Supported          |
| ------- | ------------------ |
| latest  | yes                |
| older   | no                 |

## Reporting a Vulnerability

Email **security@bocklang.org** with:

- A description of the issue
- Steps to reproduce (or a proof-of-concept)
- The Bock version and platform
- Your name / handle for credit (optional)

Please do not open a public issue for vulnerability reports.

## Disclosure Policy

- We acknowledge reports within **3 business days**.
- We aim to ship a fix within **30 days** for high-severity issues
  and **90 days** for lower-severity ones.
- After a fix ships, we credit the reporter (unless they prefer
  anonymity) in the release notes and `CHANGELOG.md`.
- We coordinate disclosure: a CVE and public advisory are published
  alongside the fix release.

## Scope

In scope:

- The Bock compiler (`compiler/`)
- The standard library (`stdlib/`)
- The VS Code extension (`extensions/vscode/`)
- Released binaries on GitHub Releases and crates.io

Out of scope:

- Vulnerabilities in third-party dependencies (report upstream)
- Issues in user code that the compiler accepts but produces
  incorrect output for — file these as regular bugs
- The bocklang.org marketing site (file as a regular bug unless
  user data is at risk)
