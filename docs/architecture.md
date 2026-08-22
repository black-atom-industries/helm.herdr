# Architecture

Helm for Herdr is a picker center for choosing a destination or Herdr action. Herdr owns the presentation: `helm-herdr.open` opens a session-modal popup, while `helm-herdr.open-side` opens the persistent Side pane.

## Runtime shape

```text
Herdr keybinding
  -> plugin action: helm-herdr.open
  -> Herdr opens a 90% × 90% popup
  -> binary runs: helm-herdr ui
  -> collect live sources
  -> project Query into Topology or flat Result list
  -> Enter applies the selected Entry action
```

The default popup dimensions are configurable with integer `picker.popup_width` and `picker.popup_height` values from 1 through 100. The Side pane uses a Herdr split and does not use popup dimensions.

## Entry points

| Command | Purpose |
| --- | --- |
| `helm-herdr open` | Ask Herdr to open the configured popup |
| `helm-herdr open-side` | Launch, focus, or close the Side pane according to its toggle state |
| `helm-herdr ui` | Run the interactive Picker |
| `helm-herdr list` | Print collected Entries without opening the Picker |

## Code layout

```text
src/main.rs      CLI entrypoints and popup/Side requests
src/app.rs       Picker state, Query projection, filtering, and Entry actions
src/tui.rs       terminal UI and command execution
src/keymap.rs    key registry, labels, groups, and active states
src/config.rs    plugin config loading and defaults
src/model.rs     Source/Entry/EntryAction models
src/sources.rs   source collectors
src/topology.rs  live Workspace/Tab/Pane model and four-line blocks
src/herdr.rs     Herdr CLI and socket calls
```

## Query projections

The projections are exclusive:

- An empty Query with no Source filter or the Workspace Source filter shows the live Topology. Each Workspace is one fixed four-line block: Workspace identity, `tabs`, the selected Tab's `panes`, and selected Pane detail.
- A typed Query, or any non-Workspace Source filter, shows a flat Result list. Each row is one terminal line in the order `Source | symbol+word status | destination`.

The exact `agent` Query token scopes results to agent Entries. `!name` narrows agent names, `@text` matches agent workspace or status text, `/path` matches paths, and `#status` matches status text. Marked Entries use the visible `bookmark` Source while keeping one destination row.

## Sources and Enter actions

| Source | Enter action |
| --- | --- |
| `workspace` | Focus the exact Workspace ID |
| `tab` | Focus the exact Tab ID |
| `pane` | Call Herdr `pane.focus` with the exact `pane_id` |
| `agent` | Focus the agent target through Herdr |
| `project` | Reuse or create a Workspace and apply the project tabs and splits |
| `server` | Hand off to the configured remote Herdr target |
| `zoxide` / `root` | Reuse or create a directory Workspace; `Alt-Enter` applies the directory template |
| `session` | Run the configured session command |
| `quick` | Invoke the Herdr Plus Quick Actions picker |
| `plugin` | Run the integration's configured open command |
| `bookmark` | Apply the marked Entry's underlying action |

Topology navigation uses Workspace, Tab, and Pane depth. `Up`/`Down` move through Workspaces and return from child depth; `Right` enters or advances child selections; `Left` returns to the parent depth; `Tab`/`Shift-Tab` advance or move back within the active child depth. `[` and `]` move between Workspaces from any depth. Enter always applies the action for the exact selected Workspace, Tab, or Pane.

## Side behavior

The Side pane is a persistent Herdr split. `helm-herdr.open-side` opens it when absent, focuses it when present but unfocused, and closes it when already focused. Enter does not close the Side pane, so it remains available after a successful action.

## Herdr Plus boundary

Helm integrates with Herdr Plus without copying its UI:

- project Entries read project TOML and create or reuse Workspaces, tabs, and splits;
- the Quick Action Entry delegates to Herdr Plus.

## Configuration boundary

Current Picker keys are `reuse_existing`, `create_missing`, `engine`, `source_order`, `source_priority_boost`, `agent_sort`, `popup_width`, `popup_height`, `check_updates`, `directory_template`, `directory_template_key`, and `[picker.filter_keys]`. Other current sections are `[notifications]`, `[jump_back]`, `[sources]`, `[theme]`, `[[roots]]`, `[[agent_aliases]]`, `[[sessions.entries]]`, and `[[integrations]]`.

## Theme boundary

Herdr plugin APIs do not expose the active palette directly. Helm reads the Herdr config, maps supported theme names locally, applies custom tokens, and falls back to One Light.

## Design goals

- One Picker for Herdr destinations and actions.
- Exact Herdr identity for Workspace, Tab, and Pane actions.
- Optional sources degrade quietly.
- Small Rust binary with no external Picker dependency.
