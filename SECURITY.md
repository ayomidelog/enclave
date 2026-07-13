# Security Policy

## Supported Versions

Enclave is under active hardening and maintenance. The `main` branch and latest tagged release are considered supported.

## Reporting a Vulnerability

Please report security issues privately by opening a GitHub Security Advisory draft in this repository.

Include:

- Enclave version / commit hash
- Host distro + kernel version
- Exact command sequence to reproduce
- Impact assessment (what can be read/written/executed)
- Any proof-of-concept code or logs

Do not open public issues for unpatched vulnerabilities.

## Response Process

1. Maintainer acknowledges the report.
2. Impact and exploitability are triaged.
3. A fix is developed and validated (`fmt`, `check`, `clippy`, `test`).
4. A coordinated disclosure and patch release is published.

## Security Scope

For the current threat model, workspace storage exposure, setup-command caveats, and operational guidance, see [docs/security.md](docs/security.md).

For product constraints that affect deployment expectations, see [docs/limitations.md](docs/limitations.md).

The current runtime hardening model includes user-namespace isolation, capability dropping, seccomp deny rules, read-only `/proc/sys` and `/sys` remounts, per-workspace network isolation, and optional AppArmor/SELinux integration hooks. Host policy definitions and setup-command trust remain operational concerns documented in [docs/security.md](docs/security.md).
