# Architecture

Helm for Herdr is a picker center for choosing a destination or Herdr action. The `helm-herdr` binary runs inside a user-owned session-modal popup, while `helm-herdr.open-side` opens the persistent Side pane.

## Runtime shape

```text
Herdr keybinding
  -> user-owned popup: helm-herdr
  -> Herdr opens the configured dimensions
  -> binary runs its Picker UI
  -> collect live sources
  -> project Query into the glance or a flat Result list
  -> Enter applies the selected Entry action
```

Popup dimensions belong to the user's Herdr `type = "popup"` keybinding. The Side pane uses a Herdr split and does not use popup dimensions.

## Entry points

| Command | Purpose |
| --- | --- |
| `helm-herdr open` | Ask Herdr to open the configured popup |
| `helm-herdr open-side` | Launch, focus, or close the Side pane according to its toggle state |
| `helm-herdr` | Run the interactive Picker; `helm-herdr ui` remains an explicit equivalent |
| `helm-herdr list` | Print collected Entries without opening the Picker |

## State files and events

Helm keeps its state in the plugin config directory (`HERDR_PLUGIN_CONFIG_DIR`).

| File | Written by | Holds |
| --- | --- | --- |
| `recent-workspaces.json` | Picker and jump back | Workspace IDs per Herdr session, most recent first |
| `focused-workspace`, `jump-back-workspace` | `track-focus` | The last focused Workspace and the Previous workspace |
| `activity.json` | `track-focus`, `track-agent` | When each Workspace was last focused and when each Pane's agent status last changed |
| `pinned-entries.json` | Picker (`^B`) | Marked Entries, which sit in the Pinned group |

`activity.json` has this shape:

```json
{
  "version": 1,
  "workspaces": { "w1": { "focused_at": 1792931924 } },
  "panes": { "w1:p1": { "status": "working", "since": 1792931924 } }
}
```

Times are Unix seconds. `since` changes only when the agent status changes. Opening the Picker drops Workspaces and Panes missing from the live Topology, and skips pruning when the Topology is empty. Each write replaces the file through a temporary file and a rename.

Herdr runs these commands as event hooks from `herdr-plugin.toml`. Each receives the event envelope as JSON in `HERDR_PLUGIN_EVENT_JSON`:

| Event | Command | Fields read |
| --- | --- | --- |
| `workspace.focused` | `helm-herdr track-focus` | `data.workspace_id` |
| `pane.agent_status_changed` | `helm-herdr track-agent` | `data.pane_id`, `data.agent_status` |

## Code layout

```text
src/main.rs      CLI entrypoints and popup/Side requests
src/app.rs       Picker state, Query projection, filtering, and Entry actions
src/tui.rs       terminal UI and command execution
src/keymap.rs    key registry, labels, groups, and active states
src/activity.rs  activity.json state and event payload parsing
src/config.rs    plugin config loading and defaults
src/model.rs     Source/Entry/EntryAction models
src/sources.rs   source collectors
src/glance.rs    glance grouping, row signals, ages and status icons
src/detail.rs    detail pane git reads and Claude subagent transcripts
src/topology.rs  live Workspace/Tab/Pane model and Git identity
src/herdr.rs     Herdr CLI and socket calls
src/recent.rs   recent Workspace order and the Previous workspace per Herdr session
src/paths.rs     plugin config and state file paths
src/matcher.rs   Query matching engines
src/theme.rs     palette, inherited from Herdr when enabled
src/update.rs    GitHub release check and update install
src/integrations/ built-in Herdr Plus adapter and command integrations
```

## Query projections

The projections are exclusive:

- With no Source filter or the Workspace Source filter, the Picker shows the glance. A typed Query narrows its rows in place and keeps the group layout.
- Any other Source filter shows a flat Result list. Each row is one terminal line in the order `destination | Source | symbol+word status`, with Source and status pinned to the right edge.

While a Query is active, quick action and plugin integration Entries match in an Other group after Directories. Without a Query they appear only under their Source filter.

The exact `agent` Query token scopes results to Entries with an agent. `!name` narrows agent names, `@text` matches workspace or status text, `/path` matches paths, and `#status` matches status text. In the glance, `agent`, `!name`, `@text` and `#status` match a Workspace row when one of its agent panes matches the agent name or status. Marked Entries keep the visible `bookmark` Source label.

## Glance

The glance has one row per open Workspace, in five Warmth groups:

- **Pinned**: Marked Workspaces only.
- **Needs you**: an agent that is waiting, blocked or done.
- **Active**: a working agent, or activity within `active_minutes`.
- **Recent**: activity within `recent_hours`.
- **Quiet**: everything else.

Activity is the newest of the Workspace's focus time, its last commit, and the newest agent status change whose status still matches the live state. Each group lists its rows by newest activity first. The Signal column shows only the age of that activity, such as `4m` or `9d`, left-aligned like the other columns. The detail pane names the sources, such as `last commit 6m · focused 38m`.

`here` marks the focused Workspace, and `last` marks the Previous workspace. Search adds a Directories group for root and zoxide matches, and an Other group for quick action and plugin matches, both only while a Query is active. The glance reloads the Herdr snapshot and activity every 3 seconds on a background thread, and reads git identities at open and then every 30 seconds. It keeps the selection on the same Workspace.

The detail pane sits beside the list when the popup is at least 110 columns wide, and takes 45% of the width. It shows the selected Workspace's name, branch and path, numbered tab chips with the focused tab filled, then its agents, uncommitted changes and recent commits. A main agent shows its state and the time since that state began. Running Claude subagents appear under their agent, with a `PROGRESS: n/m` bar read from their transcripts in `~/.claude/projects`; Helm only reads those files. The list's progress bar is the last column of a row. It sums the running subagents of a Workspace into an 8-cell bar, and shows only while the Workspace has a working agent.

Changes and commits come from `git status`, `git diff --numstat` and `git log` on a background thread. Each Workspace's git read is kept for 10 seconds. The changes and commits lists each show up to 6 rows and share one scroll offset, which `Shift-J` and `Shift-K` move. The offset resets when the selection moves to another row.

## Sources and Enter actions

| Source | Enter action |
| --- | --- |
| `workspace` | Focus the exact Workspace ID |
| `zoxide` / `root` | Reuse or create a directory Workspace; `Alt-Enter` applies the directory template |
| `quick` | Invoke the Herdr Plus Quick Actions picker |
| `plugin` | Run the integration's configured open command |
| `bookmark` | Apply the marked Entry's underlying action |

In Normal mode, `j`/`k` and `Up`/`Down` move through the visible rows across groups. The first visible Workspace rows carry the letters from `[glance] labels` (`asdfg` by default, never `j` or `k`), and pressing one opens that row. Search mode hides the labels, and they return after `Esc`.

`Enter` opens the selected Workspace at its last active Tab, and `1`–`9` open that Tab of the selected Workspace. `]` jumps to the first row of the next group. `[` jumps to the first row of the current group, or to the previous group's first row when it is already on that row. `!` jumps to the next row that needs you, including pinned rows. `-` opens the Previous workspace. `Shift-J` and `Shift-K` scroll the detail pane, and the offset resets when the selection moves to another row.

## Side behavior

The Side pane is a persistent Herdr split. `helm-herdr.open-side` opens it when absent, focuses it when present but unfocused, and closes it when already focused. Enter does not close the Side pane, so it remains available after a successful action.

## Herdr Plus boundary

Helm integrates with Herdr Plus without copying its UI:

- directory templates read Herdr Plus project TOML and apply its tabs and splits to directory Workspaces;
- the Quick Action Entry delegates to Herdr Plus.

## Configuration boundary

Current Picker keys are `reuse_existing`, `create_missing`, `engine`, `source_order`, `source_priority_boost`, `check_updates`, `directory_template`, `directory_template_key`, and `[picker.filter_keys]`. Other current sections are `[theme]`, `[notifications]`, `[jump_back]`, `[glance]`, `[sources]`, `[[roots]]`, and `[[integrations]]`. `[glance]` holds `active_minutes`, `recent_hours`, `icons` and `labels`.

## Color boundary

Helm uses the terminal's default colors and ANSI palette. Color selection is part of the Picker renderer rather than plugin configuration.

## Design goals

- One Picker for Herdr destinations and actions.
- Exact Herdr identity for Workspace, Tab, and Pane actions.
- Optional sources degrade quietly.
- Small Rust binary with no external Picker dependency.
