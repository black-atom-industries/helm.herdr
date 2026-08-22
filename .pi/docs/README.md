# Agent Docs: Helm for Herdr

Purpose: compact project-only docs for future agents. Prefer intent over line-by-line code notes.

## Project intent

Helm for Herdr is a Herdr picker-center plugin for one fast command-palette flow:

```text
prefix+t -> search -> Enter -> land in the right place
```

It should unify:
- open Herdr workspaces
- Herdr Plus project templates
- Herdr Plus Quick Actions
- zoxide directories
- configured root scans
- agent panes

Core UX: the default action opens a session-modal popup sized at 90% × 90%; the Side action keeps a persistent split available after Enter.

## Current public identity

- Cargo package / binary: `helm-herdr`
- Herdr plugin id: `helm-herdr`
- Main action: `helm-herdr.open`
- Popup entrypoint: `picker`
- Plugin manifest: `herdr-plugin.toml`
- Main code: `src/main.rs`
- Default config template: `examples/default-config.toml`

## Fast checks

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build --release
./target/release/helm-herdr list
```

## Docs map

- `architecture.md`: runtime flow and data model
- `features.md`: current feature behavior and UX intent
- `decisions.md`: durable decisions; do not casually reverse
- `bugs-and-lessons.md`: bugs hit during development and fixes
- `release.md`: publish/release notes for agents
