# Agent Project Memory

Compact, agent-only operating context for Helm for Herdr.

## Reading order

1. This file for local workflow conventions.
2. `.agents/product-marketing.md` only for product positioning or marketing work.
3. Source code and `herdr-plugin.toml` remain authoritative for implementation behavior.

## Design

The glance view and its keymap are designed on the canvas https://claude.ai/artifact/C6Sf6rv3FMeJYUXDr49swT (board C, keymap, marks, icons). The code is authoritative where they differ: key hints use ASCII names, bars use block elements, and the list's last columns are a plain age and the progress bar.

## Local test build

When the user says **"build local"** or equivalent:

```bash
cargo build --release
herdr plugin link "$PWD"
```

This means Herdr must use the repository's `target/release/helm-herdr` through a local plugin link. Do not interpret it as only building the target binary or installing it into `~/.cargo/bin`. Verify `herdr plugin list` reports `helm-herdr` as `local:$PWD`.
