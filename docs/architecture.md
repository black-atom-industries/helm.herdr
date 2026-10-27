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
| `helm-herdr status` | Print one line for Herdr's tab bar: other running sessions with Workspaces in Needs you |
| `helm-herdr shell [session]` | Launcher: run `herdr session attach` in the terminal and attach another session when Helm requests a switch |

## State files and events

Helm keeps its state in the plugin config directory (`HERDR_PLUGIN_CONFIG_DIR`).

| File | Written by | Holds |
| --- | --- | --- |
| `recent-workspaces.json` | Picker and jump back | Workspace IDs per Herdr session, most recent first |
| `focused-workspace`, `jump-back-workspace` | `track-focus` | The last focused Workspace and the Previous workspace |
| `activity.json` | `track-focus`, `track-agent`, Picker, `shell` | When each Workspace was last focused, when each Pane's agent status last changed, and when the User was last in each Herdr session |
| `pinned-entries.json` | Picker (`^B`) | Marked Entries, which sit in the Pinned group |
| `cold-entries.json` | Picker (`c`) | Workspaces sent to the Cold group, as a map from Entry key to the Unix seconds when it was sent; pinning one takes it out. A plain array of keys reads as sent at 0 |
| `launchers/<session>.json` | `shell` | The Launcher attached to that session: `launcher_pid`, `client_pid`, `session` |
| `launchers/switch-<launcher_pid>.json` | Picker | A switch request for that Launcher: `session`, optional `workspace_id` and `tab_id` |
| `launchers/last-session` | `shell` | The session the last Launcher ran a client of until it ended cleanly or switched |

`activity.json` has this shape:

```json
{
  "version": 1,
  "workspaces": { "work::w1": { "focused_at": 1792931924 } },
  "panes": { "work::w1:p1": { "status": "working", "since": 1792931924 } },
  "sessions": { "work": { "seen_at": 1792931924 } }
}
```

Keys are the Herdr session name (`<default>` for the default session), `::`, and the Herdr ID, because Herdr IDs repeat across sessions. The hooks read the session from `HERDR_SESSION`, like the Picker. Times are Unix seconds. `since` changes only when the agent status changes. `sessions` holds `seen_at` per session (`<default>` for the default session), set by `track-focus`, by opening the Picker, and by the Launcher when it attaches the session and when that client exits. A session without an entry falls back to the newest `focused_at` of its Workspaces.

Opening the Picker drops the current session's Workspaces and Panes missing from its own live Topology and leaves other sessions alone. It skips pruning when the Topology has no Workspaces, and keeps every Pane when it has no Panes. Each change reloads the file under an exclusive lock on `activity.json.lock`, then replaces the file through a temporary file and a rename.

Herdr runs these commands as event hooks from `herdr-plugin.toml`. Each receives the event envelope as JSON in `HERDR_PLUGIN_EVENT_JSON`:

| Event | Command | Fields read |
| --- | --- | --- |
| `workspace.focused` | `helm-herdr track-focus` | `data.workspace_id` |
| `pane.agent_status_changed` | `helm-herdr track-agent` | `data.pane_id`, `data.agent_status` |

## Herdr sessions

Helm finds the Herdr root at `$XDG_CONFIG_HOME/herdr`, or `~/.config/herdr` when `XDG_CONFIG_HOME` is unset. The plugin config directory falls back to `<root>/plugins/config/helm-herdr` when Herdr does not set `HERDR_PLUGIN_CONFIG_DIR`.

The sessions are the default session, whose directory is the root, and every directory in `<root>/sessions/`, symlinks followed. Each session's socket is `herdr.sock` in its directory. A session runs when its socket answers a `session.snapshot` request within 300 ms; the sockets are asked in parallel, so a dead one costs at most one timeout. The current session is `HERDR_SESSION`, unset for the default session, which Herdr also names `default`.

Each load, at open and in the 3-second background refresh, reads the snapshot of every other running session and keeps only the Workspaces where an agent is blocked or done. Herdr tracks focus per session, so an agent finishing in a session the User has left still sits on a focused Pane and reads `idle`. In another session, an `idle` agent whose `since` is newer than that session's `seen_at` therefore counts as done; with no seen time it stays idle. The current session's Workspaces keep Herdr's state. Only those get git and transcript reads. Their rows join the glance in Needs you, classified with their own session's `activity.json` entries; pinned and cold state never applies to them, and they never count as the Current or Previous workspace. The row shows an `@<session>` tag in the mauve colour inside the name cell, and the header adds ` · M elsewhere` to the need-you count. The footer shows `enter switch to <session>` while such a row is selected. Opening it (`enter`, label twice, or `1`–`9` for one of its Tabs) asks the Launcher of the current session to switch to that session, Workspace and Tab, and closes the popup (the Side pane stays). When the current session has no live Launcher record, the Picker stays open with `Start Herdr with helm-herdr shell to switch sessions` in the footer until the next key. Close, Cold and pin ignore these rows, because Workspace IDs repeat across sessions.

The same discovery feeds the Sessions view. Running lists the current session first, then the other running sessions by their newest Workspace focus or agent status change in `activity.json`, in discovery order otherwise; Stopped lists the sessions whose socket did not answer. A row shows the most urgent of blocked, done and working across the session's Workspaces, `N need you` in the colour of the most urgent Needs you state, the Workspace count (`start` for a stopped session) and the age of the newest activity. The detail pane classifies the session's Workspaces with the glance's rules and that session's activity, without Pinned, and with Cold by hand only in the current session; it groups them into Needs you, Hot, Warm and Cold, newest activity first, numbers the first nine across groups, and lists the session's agents. Other sessions' Workspaces get git through a background read of the selected session, kept for 30 seconds; until it lands, a Workspace outside Needs you shows no branch and is classified without its last commit.

Opening another session's row switches the same way: `enter` or its label twice targets the session's most recently focused Workspace from `activity.json` (none when unknown, and a stopped session starts on attach), and `1`–`9` targets that numbered Workspace in the detail list; a digit past the list does nothing. On the current session's row, `enter` returns to the glance and `1`–`9` focuses that Workspace. Close, Cold and pin ignore session rows.

`helm-herdr status` reads the same snapshots and `activity.json` without git or transcript reads, counts each session's Workspaces in Needs you with the glance's classification, the derived done included, and prints `<session> <icon><count>` per session in discovery order, joined by two spaces. The icon is that of the most urgent state (blocked before done) in the configured icon style. The output is plain text without a trailing newline, empty when nothing waits, and the exit code is always 0.

### Launcher

`helm-herdr shell [session]` attaches the argument, else the session in `launchers/last-session`, else `default`, with `herdr session attach <session>` (`herdr` from `PATH`, or `HERDR_BIN_PATH`), which starts a stopped session. A name that is empty, contains `/` or `..`, or starts with `switch-` is refused with exit code 2. The launcher runs the client as a child with the terminal inherited, writes `launchers/<session>.json` and waits. It ignores SIGINT while it waits: the client reads Ctrl-C as a key in raw mode, and a stray SIGINT must not end the launcher before its client.

When the client exits, the launcher removes its record, unless another launcher has written that session's record since, and takes `launchers/switch-<launcher_pid>.json` if present. It writes the session to `launchers/last-session` when the client exited with status 0, was ended by a signal, or a switch request follows, so a session that failed to attach is not remembered. It runs `stty sane`, shows the cursor and turns off mouse tracking, bracketed paste, focus reports and the kitty keyboard mode. It leaves the alternate screen only when a request follows or a signal ended the client, because leaving it also restores the cursor over a client's parting message.

A request names the next session. The launcher attaches it and, when the request names a Workspace, focuses it (and its Tab) through that session's socket from a background thread, retrying for up to 5 seconds while a started session comes up. Without a request the launcher exits with the client's status. At start it drops a request left under its own pid.

A record is live while its client pid is a running process named `herdr` (`ps -o ppid=,comm=`) whose parent is the recorded launcher; a client whose launcher died has another parent, so it counts as stale too. Switching therefore needs the Herdr binary to be named `herdr`. A switch writes the request first, then checks that the client is the launcher's live `herdr` child, and only then sends the client SIGTERM; a stale record withdraws the request. A request that cannot be written or a client that cannot be signalled shows `Switching to <session> failed` in the footer. Herdr has no client-detach API, and the session server keeps running after its client exits. One record per session, last writer wins.

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
src/sessions.rs  Herdr session discovery, other sessions' snapshots, Launcher records and switch requests
src/launcher.rs  `helm-herdr shell`: attach loop, terminal restore and focus after a switch
src/recent.rs   recent Workspace order and the Previous workspace per Herdr session
src/paths.rs     Herdr root, plugin config and state file paths
src/matcher.rs   Query matching engines
src/theme.rs     palette, inherited from Herdr when enabled
src/update.rs    GitHub release check and update install
src/integrations/ built-in Herdr Plus adapter and command integrations
```

## Query projections

The projections are exclusive:

- With no Source filter or the Workspace Source filter, the Picker shows the glance. A typed Query narrows its rows in place and keeps the group layout. With the nucleo engine, a row stays only while its match score reaches 85% of the best visible score, before Source bonuses; Queries made only of tokens keep every matching row. Plain text matches what a row shows, never the agent names and statuses that only Query tokens read. The cursor lands on the best match, Source bonus included, and follows it as the Query changes. Once the user moves the cursor, it stays on that row while the row is visible.
- The Session Source filter (`^S`) shows the Sessions view, laid out like the glance: one row per Herdr session in a Running and a Stopped group, beside a detail pane. A typed Query narrows the rows by session name and keeps their order.
- Any other Source filter shows a flat Result list. Each row is one terminal line in the order `destination | Source | symbol+word status`, with Source and status pinned to the right edge.

While a Query is active, quick action and plugin integration Entries match in an Other group after Directories. Without a Query they appear only under their Source filter.

The exact `agent` Query token scopes results to Entries with an agent. `!name` narrows agent names, `@text` matches workspace or status text, `/path` matches paths, and `#status` matches status text. In the glance, `agent`, `!name`, `@text` and `#status` match a Workspace row when one of its agent panes matches the agent name or status. Marked Entries keep the visible `bookmark` Source label.

## Glance

The glance has one row per open Workspace, in five Warmth groups:

- **Pinned**: Marked Workspaces only.
- **Needs you**: an agent that is waiting, blocked or done.
- **Hot**: a working agent, or activity within `hot_minutes`.
- **Warm**: activity within `warm_hours`.
- **Cold**: everything else, and Workspaces sent there with `c` whose newest activity is not newer than the moment they were sent. Activity after that moment classifies the Workspace by its age again; its entry stays in `cold-entries.json` without effect until `c` `c` sends it to Cold again or pinning removes it. A cold Workspace still rises to Needs you when an agent waits on you.

Activity is the newest of the Workspace's focus time, its last commit, and the newest agent status change whose status still matches the live state. Opening the Picker lists each group's rows by newest activity first. The Signal column shows only the age of that activity, such as `4m` or `9d`, left-aligned like the other columns. The detail pane names the sources, such as `last commit 6m · focused 38m`. While the Picker stays open, a refresh keeps every row and its label in place: a row moves only when it changes group, new rows join the end of their group, and closed Workspaces drop out.

Search adds a Directories group for root and zoxide matches, and an Other group for quick action and plugin matches, both only while a Query is active. The glance reloads the Herdr snapshot and activity every 3 seconds on a background thread, and reads git identities at open and then every 30 seconds. Each identity read also runs `git status --porcelain=v2 --branch` in the Workspace and keeps its staged, modified and untracked counts and its ahead and behind counts; the GIT column shows them as `+n !n ?n ↑n ↓n` without the zero parts, a green `✓` when all are zero, and nothing before the first read. Ahead and behind need an upstream. The column takes at most 12 cells and cuts its text from the right; the branch column gives up width first. It keeps the selection on the same Workspace.

The detail pane sits beside the list when the popup is at least 110 columns wide, and takes 45% of the width. It shows the selected Workspace's name, branch and path, numbered tab chips with the focused tab filled (chips that do not fit drop from the right, so the name always shows), then its agents, uncommitted changes and recent commits. A main agent shows its state and the time since that state began. Running Claude subagents appear under their agent, with a `PROGRESS: n/m` bar read from their transcripts in `~/.claude/projects`; Helm only reads those files. `n/m` marks the start of step `n`, so a subagent on `m/m` still runs. A subagent has finished when its transcript ends with a turn that stops without a tool call, the result of its `SubagentHandback` call, or an interruption. Transcripts untouched for 10 minutes count as abandoned. Every agent always shows; when the pane is short, the subagents that do not fit collapse into a `└ +N more` line. The list's progress bar is the last column of a row. It sums the running subagents of a Workspace into an 8-cell bar, and shows only while the Workspace has a working agent.

Changes and commits come from `git status`, `git diff --numstat` and `git log` on a background thread. Each Workspace's git read is kept for 10 seconds. The changes and commits lists each show up to 6 rows, fewer when the pane is short or the agents need the room, and share one scroll offset, which `Shift-J` and `Shift-K` move. The offset resets when the selection moves to another row.

## Sources and Enter actions

| Source | Enter action |
| --- | --- |
| `workspace` | Focus the exact Workspace ID |
| `session` | Switch to that Herdr session; for the current session, return to the glance |
| `zoxide` / `root` | Reuse or create a directory Workspace; `Alt-Enter` applies the directory template |
| `quick` | Invoke the Herdr Plus Quick Actions picker |
| `plugin` | Run the integration's configured open command |
| `bookmark` | Apply the marked Entry's underlying action |

Helm opens with the Current workspace selected, or the first Workspace row when it is not listed. In Normal mode, `j`/`k` and `Up`/`Down` move through the visible rows across groups. The first visible Workspace rows carry the letters from `[glance] labels` (`asdfg` by default, never `c`, `j`, `k`, `l`, `q` or `x`), and pressing one selects that row. Pressing the same label again right away opens it, so `aa` opens the first row and `a3` its third Tab. Search mode hides the labels, and they return after `Esc`.

`Enter` opens the selected Workspace at its last active Tab, and `1`–`9` open that Tab of the selected Workspace. `]` jumps to the first row of the next group. `[` jumps to the first row of the current group, or to the previous group's first row when it is already on that row. `!` jumps to the next row that needs you, including pinned rows. With a live Launcher for the current session, `l` switches to another session's Workspace when it was focused more recently than any local one (newest `focused_at` in `activity.json` over running sessions, never the current Workspace), else opens the Previous workspace, also from the Sessions view. `jump-back-workspace` and the `jump-back` action stay per session. `q` closes Helm. `^S` shows the Sessions view, where the labels mark its first rows and `1`–`9` pick the selected session's numbered Workspaces. `x` arms closing the selected Workspace and the footer asks for a second `x`; any other key cancels, and `^X` closes at once. `c` arms sending the selected Workspace to Cold and a second `c` confirms; on a cold Workspace a single `c` warms it up again. `Shift-J` and `Shift-K` scroll the detail pane, and the offset resets when the selection moves to another row.

## Side behavior

The Side pane is a persistent Herdr split. `helm-herdr.open-side` opens it when absent, focuses it when present but unfocused, and closes it when already focused. Enter does not close the Side pane, so it remains available after a successful action.

## Herdr Plus boundary

Helm integrates with Herdr Plus without copying its UI:

- directory templates read Herdr Plus project TOML and apply its tabs and splits to directory Workspaces;
- the Quick Action Entry delegates to Herdr Plus.

## Configuration boundary

Current Picker keys are `reuse_existing`, `create_missing`, `engine`, `source_order`, `source_priority_boost`, `check_updates`, `directory_template`, `directory_template_key`, and `[picker.filter_keys]`. Other current sections are `[theme]`, `[notifications]`, `[jump_back]`, `[glance]`, `[sources]`, `[[roots]]`, and `[[integrations]]`. `[glance]` holds `hot_minutes`, `warm_hours`, `icons` and `labels`. A `source_order` without `session` places the Sessions view's filter key right after the workspace key.

## Color boundary

Helm uses the terminal's default colors and ANSI palette. Color selection is part of the Picker renderer rather than plugin configuration.

## Design goals

- One Picker for Herdr destinations and actions.
- Exact Herdr identity for Workspace, Tab, and Pane actions.
- Optional sources degrade quietly.
- Small Rust binary with no external Picker dependency.
