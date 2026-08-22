# Feature Intent

Helm is one Picker for Herdr destinations and actions. The common flow is `prefix+t -> type -> Enter`.

## Presentation

- `open` requests a session-modal Herdr popup. It defaults to 90% × 90%, controlled by `picker.popup_width` and `picker.popup_height` as integer percentages from 1 through 100.
- `open-side` opens a persistent right split, focuses it when it exists elsewhere in the Workspace, and closes it when already focused. Enter leaves the Side pane open.

## Result projections

An empty Query with no Source filter or the Workspace Source filter renders the live Topology as fixed four-line blocks for each Workspace. The four lines contain the Workspace identity, its `tabs`, the selected Tab's `panes`, and the selected Pane detail.

A typed Query or any non-Workspace Source filter renders a flat Result list. Every row is one terminal line: `Source | symbol+word status | destination`. Topology and flat projections never mix. Marked Entries display Source `bookmark` without duplicate rows.

Topology navigation has Workspace, Tab, and Pane depth. Arrow keys and `h`/`j`/`k`/`l` move through the active depth; `Right` enters or advances children, `Left`/`h` returns to the parent depth, `Tab`/`Shift-Tab` advances or moves back within the active child depth, and `[`/`]` changes Workspace. Enter focuses the exact selected Workspace, Tab, or Pane ID. The standalone Query token `agent` scopes results to agent Entries; `!name`, `@workspace-or-status`, `/path`, and `#status` narrow them further.

## Sources

```toml
["workspace", "agent", "project", "session", "zoxide", "root", "server", "quick", "plugin"]
```

Visible flat Sources include `workspace`, `tab`, `pane`, `agent`, `project`, `server`, `session`, `zoxide`, `root`, `quick`, `plugin`, and `bookmark`.

## Enter actions

- Workspaces, Tabs, and Panes focus their exact Herdr IDs. Pane actions use Pane IDs directly.
- Agent Entries focus the Herdr agent target.
- Project, zoxide, and root Entries reuse or create Workspaces. `Alt-Enter` applies the configured directory template to zoxide/root Entries.
- Server Entries hand off through Herdr's remote flow.
- Quick Actions invoke Herdr Plus; plugin Entries run their configured command; session Entries run their configured session command.

## Other behavior

`picker.check_updates` performs a non-blocking daily release check and shows a release notice when one is available. Jump Back records successful local Workspace transitions when enabled. Source shortcuts are configurable through `[picker.filter_keys]`, and optional sources degrade quietly when their dependencies are absent.

## Remote handoff

Helm hands remote targets to Herdr rather than wrapping an SSH terminal. Open reads the current local session.

## Herdr Plus

Project Entries reuse or create a Workspace and apply project tabs and panes. Quick Actions remain owned by Herdr Plus.

## Theme

Helm maps Herdr theme names locally and applies `[theme.custom]` overrides because Herdr plugin APIs do not expose the active palette.

## Command/JSON plugin integrations

Users add external tools with `[[integrations]]` entries containing `collect` and `open` commands. Collect JSON requires `id` and `title`; `subtitle`, `path`, and `kind` are optional.
