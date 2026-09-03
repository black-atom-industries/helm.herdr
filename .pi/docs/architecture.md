# Architecture Notes

Helm is a Rust TUI Herdr plugin. `main` handles CLI entrypoints and Herdr placement, `app` owns Picker state and Entry actions, `tui` renders and reads input, and `topology` models the live Workspace → Tab → Pane structure.

## Entry modes

- `open` requests the configured session-modal popup.
- `open-side` manages the persistent Side split.
- `ui` runs the interactive Picker.
- `list` prints collected Entries for debugging.

The `helm-herdr` binary runs in a popup whose dimensions belong to the user's Herdr `type = "popup"` keybinding. The Side split has no popup dimensions and remains open after Enter.

## Data flow

1. Load the plugin config.
2. Collect enabled source Entries and the live Workspace/Tab/Pane Topology.
3. With an empty Query and no filter or the Workspace filter, render fixed four-line Workspace blocks.
4. With a typed Query or any non-Workspace filter, score and render a flat one-line Result list.
5. Enter applies the selected Entry action.

## Projections

Topology blocks show one Workspace as exactly four terminal lines: Workspace identity, `tabs`, the selected Tab's `panes`, and selected Pane detail. Workspace, Tab, and Pane depth are navigated independently, and Enter focuses the exact selected identity. Pane focus uses the exact Pane ID.

Flat Query rows use `Source | symbol+word status | destination`. The exact `agent` Query token scopes results to agent Entries. `!name`, `@workspace-or-status`, `/path`, and `#status` provide the other agent and path predicates. A marked Entry displays Source `bookmark` and remains one row.

## Source actions

- `workspace`: focus the exact Workspace ID.
- `tab`: focus the exact Tab ID.
- `pane`: call Herdr `pane.focus` with the exact `pane_id`.
- `agent`: focus the agent target.
- `project`: reuse or create a Workspace and apply project tabs and splits.
- `server`: hand off to the configured remote target.
- `zoxide` and `root`: reuse or create a directory Workspace; `Alt-Enter` applies the directory template.
- `session`: run the configured session command.
- `quick`: invoke Herdr Plus Quick Actions.
- `plugin`: run the integration's configured open command.
- `bookmark`: apply the marked Entry's underlying action.

## Herdr Plus boundary

The Herdr Plus adapter reads project TOML, creates or reuses Workspaces, and applies tabs, panes, labels, and startup commands. Quick Actions remain owned by Herdr Plus; Helm invokes that Picker.

## Integration layer

Command/JSON integrations provide a `collect` command that prints an array and an `open` command that runs for the selected Entry. Helm owns success and error notifications. `Source` identifies the visible category; `EntryAction` owns Enter behavior.

## Color flow

Helm loads Herdr's configured palette by default, including light/dark and custom color settings. The Picker renderer derives group, depth, and cell backgrounds from that palette. Setting `[theme].inherit_herdr = false` uses Helm's built-in light palette.
