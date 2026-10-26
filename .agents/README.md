# Agent Project Memory

Compact, agent-only operating context for Helm for Herdr.

## Reading order

1. This file for local workflow conventions.
2. Source code and `herdr-plugin.toml` remain authoritative for implementation behavior.

## Designs

Design canvases (Claude Design artifacts), newest first. The code is authoritative where it and a canvas differ. Add a new canvas here with its date when a design starts; extend an existing canvas when the work continues it.

| Date | Canvas | Covers |
| --- | --- | --- |
| 2026-10-25 | [Helm glance view](https://claude.ai/artifact/C6Sf6rv3FMeJYUXDr49swT) | Glance list, detail pane, keymap, here/last marks, status icons. Row D is the glance as built on 2026-10-26; rows A–C are earlier explorations. Row E covers sessions: other sessions in Needs you only, the ^S sessions view, session mark variants, the switch flow with the tab bar segment. Row F walks the session flows as stories, screen by screen, and is the approved sessions design (Compare A: other sessions inside Needs you with an @session tag). |

## Local test build

When the user says **"build local"** or equivalent:

```bash
cargo build --release
herdr plugin link "$PWD"
```

This means Herdr must use the repository's `target/release/helm-herdr` through a local plugin link. Do not interpret it as only building the target binary or installing it into `~/.cargo/bin`. Verify `herdr plugin list` reports `helm-herdr` as `local:$PWD`.
