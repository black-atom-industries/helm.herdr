---
name: repo-dep-upgrades
description: Use when upgrading dependencies, reviewing Dependabot/Renovate PRs, or auditing outdated packages in this project.
metadata:
  argument-hint: "[package-name|all]"
---

# Dependency Upgrades

Upgrade dependencies with changelog review, verification, and safe commits.

## Arguments

The argument (`$ARGUMENTS` in Claude Code, or `/skill:repo-dep-upgrades` args in Pi) — optional package name(s) to upgrade. Empty to review all outdated deps.

## Ecosystem

- **Package manager:** Cargo
- **Lock file:** Cargo.lock
- **Outdated command:** `cargo update --dry-run --verbose` (`Unchanged ... (available: ...)` marks an upgrade outside the `Cargo.toml` range)
- **Upgrade command:** `cargo update -p`
- **Verification:** `mise run verify:full`
- **Commit prefix:** `build(deps):`

## Workflow

### 1. Survey

Run the outdated command:

```bash
cargo update --dry-run --verbose
```

### 2. Strategy Selection

- **Batch low-risk, majors one-by-one** — patch+minor together, each major separately
- **All one-by-one** — safest, upgrade and verify each individually
- **All at once** — fast, single upgrade + verify pass

### 3. Per-Upgrade Checklist

Research changelog, check usage in project (`grep` for imports), upgrade, verify, report:

```bash
cargo update -p <package>
mise run verify:full
```

`cargo update -p` stays inside the semver range in `Cargo.toml`. For a major version, raise the requirement in `Cargo.toml` first, then run the upgrade command.

Commit a green upgrade only after approval, staging the manifest, the lock file, and the code the upgrade required, as
`build(deps): upgrade <package> from X to Y`.

### 4. Summary

Upgraded list, deprecation warnings, new features, skipped deps.
