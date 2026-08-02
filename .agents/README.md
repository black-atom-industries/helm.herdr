# Agent Project Memory

Compact, agent-only operating context for Herdr Navigator.

## Reading order

1. This file for local workflow conventions.
2. `.agents/product-marketing.md` only for product positioning or marketing work.
3. Source code and `herdr-plugin.toml` remain authoritative for implementation behavior.

## Local test build

When the user says **“build để tôi test”**, **“build local”**, or equivalent:

```bash
cargo build --release
herdr plugin link "$PWD"
```

This means Herdr must use the repository's `target/release/herdr-navigator` through a local plugin link. Do not interpret it as only building the target binary or installing it into `~/.cargo/bin`. Verify `herdr plugin list` reports `herdr-navigator` as `local:$PWD`.
