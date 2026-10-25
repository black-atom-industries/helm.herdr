# Helm for Herdr

<p align="center">
  <strong>One popup to reach every workspace in Herdr, and every directory you work in.</strong>
</p>

<p align="center">
  <a href="https://github.com/black-atom-industries/helm.herdr/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/black-atom-industries/helm.herdr/actions/workflows/ci.yml/badge.svg" /></a>
  <a href="https://github.com/black-atom-industries/helm.herdr/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/black-atom-industries/helm.herdr" /></a>
  <a href="LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-2ea44f" /></a>
  <img alt="Herdr 0.7.4+" src="https://img.shields.io/badge/Herdr-0.7.4%2B-66b3ff" />
  <img alt="Linux and macOS" src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-c084fc" />
</p>

Helm opens on a glance of your open Workspaces, grouped by what needs you and how recently each one was active. Enter opens the selected Workspace at the Tab you used last. Press `/` and type, and the same list reaches zoxide directories and configured roots that are not open yet. Helm focuses what exists and creates a Workspace for what does not.

> [!NOTE]
> Part of the [Helm](https://github.com/black-atom-industries) family, next to [helm.tmux](https://github.com/black-atom-industries/helm.tmux). Forked from [thanhdat77/herdr-navigator](https://github.com/thanhdat77/herdr-navigator); thanks to thanhdat77 for the foundation.

## Install

Install the plugin. Herdr clones the repository and builds it with Cargo:

```bash
herdr plugin install black-atom-industries/helm.herdr --yes
```

Link the built binary onto your `PATH`, so a Herdr popup can run it. Run this again after every reinstall, because Herdr may use a new checkout path:

```bash
plugin_root=$(herdr plugin list --plugin helm-herdr --json | jq -r '.result.plugins[0].plugin_root')
mkdir -p "$HOME/.local/bin"
ln -sfn "$plugin_root/target/release/helm-herdr" "$HOME/.local/bin/helm-herdr"
```

Bind it in `~/.config/herdr/config.toml`. The binding owns the key and the popup size. Helm needs at least 120 × 32 cells and shows a size notice below that, so give the size in cells rather than percentages:

```toml
[[keys.command]]
key = "prefix+t"
type = "popup"
command = "helm-herdr"
description = "jump to anything"
width = 124
height = 34
```

## Glance

The glance lists each open Workspace once, in one of five groups:

- **Pinned**: Workspaces marked with `^B`, ahead of everything else.
- **Needs you**: an agent that is waiting, blocked or done.
- **Active**: a working agent, or activity within `active_minutes`.
- **Recent**: activity within `recent_hours`.
- **Quiet**: everything else.

When the Picker opens, each group lists its rows by newest activity first. A row has these columns, left to right:

| Column   | Shows                                                                                   |
| -------- | --------------------------------------------------------------------------------------- |
| label    | The `[glance] labels` letter of the first rows, which selects the row                   |
| status   | The state icon of the Workspace's agent                                                 |
| name     | The Workspace name                                                                      |
| branch   | The git branch                                                                          |
| mark     | `here` on the focused Workspace, `last` on the Previous workspace                       |
| age      | How long ago the activity that placed the row in its group happened, such as `4m`       |
| progress | While an agent works, the summed progress of its running subagents, as an 8-cell bar   |

The glance reloads Herdr's state every 3 seconds and keeps the selection on the same Workspace. Rows and their labels stay in place while the Picker is open: a row moves only when it changes group, and new rows join the end of their group. Labels are hidden while you search.

| Key                  | Action                                                                             |
| -------------------- | ---------------------------------------------------------------------------------- |
| `a` `s` `d` `f` `g`  | Select the row with that label; press it twice (`aa`) to open. Letters come from `[glance] labels` |
| `j` / `k`, `↓` / `↑` | Move to the next or previous row                                                   |
| `enter`              | Open the selected Workspace at its last active Tab                                 |
| `1`–`9`              | Open that Tab of the selected Workspace                                            |
| `]` / `[`            | Jump to the first row of the next group; `[` goes to the current group's first row, then to the previous group |
| `!`                  | Jump to the next row that needs you, wrapping around                               |
| `l`                  | Open the Previous workspace                                                        |
| `q`, `esc`           | Close Helm (`esc` first leaves search)                                             |
| `^B`                 | Pin or unpin the selected row to the top                                           |
| `x` `x`, `^X`        | Close the Workspace of the selected row; `x` asks for a second `x`, `^X` closes at once |
| `J` / `K`            | Scroll the detail pane                                                             |
| `/`                  | Search                                                                             |
| `?`                  | Show the keys sheet                                                                |
| `esc` / `^C`         | Close                                                                              |

## Detail pane

Beside the list, the detail pane describes the selected Workspace. It appears when the popup is at least 110 columns wide and takes 45% of the width. It shows:

- the Workspace name, branch and path, and numbered Tab chips with the focused Tab filled
- its agents, with the state of each and the time since that state began
- running Claude subagents under their agent, each with a `PROGRESS` bar
- uncommitted changes
- recent commits

Changes and commits each show up to 6 rows, fewer when the pane is short, and `J` and `K` scroll them together. Every agent always shows; subagents that do not fit collapse into a `└ +N more` line. The scroll resets when the selection moves to another row.

### Subagent progress

Subagent progress is read from Claude Code transcripts in `~/.claude/projects`. Helm reads those files and writes nothing. Other agents show no progress.

A subagent counts as running until its transcript ends: with a turn that stops without a tool call, its handback to the caller, or an interruption. A transcript untouched for 10 minutes counts as abandoned. The subagent writes its progress line in its assistant messages, at the start of each step:

```text
PROGRESS: 3/6 · Research markers
```

`3/6` means step 3 of 6 has begun, so `6/6` is the last step, still running.

A `SubagentStart` hook can add the instruction to each subagent, so the line appears without anyone asking for it.

## Search

Press `/` and type. Search keeps the group layout and narrows the rows in place. Rows that match far worse than the best match drop out, so `lag` leaves `lager` and not every name with an `l`, `a` and `g` in it. The cursor sits on the best match and follows it as you type, until you move it yourself. A few tokens narrow the match further:

| Token    | Matches                                                   |
| -------- | --------------------------------------------------------- |
| `agent`  | Workspaces with an agent. A bare `@` does the same        |
| `!claude` | An agent name                                            |
| `@idle`  | Workspace or status text                                  |
| `#idle`  | Status text only                                          |
| `/dotfiles` | A path                                                 |

While a Query is active, two more groups follow Quiet:

- **Directories**: zoxide and root directories. `enter` reuses or creates a Workspace there, and `alt-enter` applies the directory template.
- **Other**: Herdr Plus Quick Actions and your `[[integrations]]`. `enter` runs them.

The source filter keys switch to one source at a time. The rows then form a flat list, one line per Entry, with its Source and status on the right. `^W` returns to the glance.

| Key                 | Action                                                  |
| ------------------- | ------------------------------------------------------- |
| `^W`                | Workspaces, back to the glance                          |
| `^Z` / `^R` / `^Q`  | zoxide / roots / Quick Actions                          |
| `esc`               | Leave Search and keep the Query                         |
| `^U`                | Clear the Query and filter                              |
| `backspace`         | Delete the last Query character                         |
| `^backspace`        | Delete the last Query word                              |
| `alt-enter`         | Open a zoxide or root directory with the directory template |

`[picker.filter_keys]` remaps the source filter keys.

## More moves

### Jump back

Helm records every Workspace focus change, including the ones made with Herdr's own keys, so you can toggle between the current and the previous Workspace like tmux's last-window:

```toml
[[keys.command]]
key = "prefix+l"
type = "plugin_action"
command = "helm-herdr.jump-back"
description = "jump to previous workspace"
```

The previous Workspace carries a `last` mark in the glance. `[jump_back] enabled = false` stops the recording altogether.

### Side pane

`helm-herdr.open-side` keeps Helm in a split beside your work. It stays open after `enter`; invoking the action again focuses it, and invoking it while focused closes it.

```toml
[[keys.command]]
key = "prefix+shift+t"
type = "plugin_action"
command = "helm-herdr.open-side"
description = "helm side pane"
```

### Directory template

Point `picker.directory_template` at a Herdr Plus project file, and `alt-enter` applies its tabs, panes, labels, and commands to any zoxide or root directory. An already open Workspace gets fresh template tabs appended. `enter` keeps opening normally.

## Configuration

Helm writes a commented config on first run, in the directory this prints:

```bash
herdr plugin config-dir helm-herdr
```

[`examples/default-config.toml`](examples/default-config.toml) documents every option. A typical start:

```toml
[picker]
source_order = ["workspace", "zoxide", "root", "quick", "plugin"]
# directory_template = "default.toml"

[theme]
# Follow Herdr's palette, including auto-switching light/dark themes.
inherit_herdr = true

[sources]
zoxide = true
roots = true

[[roots]]
path = "~/code"
max_depth = 2
```

The `[glance]` section sets the group windows and the look:

```toml
[glance]
active_minutes = 15   # activity this recent places a row in Active
recent_hours = 4      # activity this recent places a row in Recent; older rows are Quiet
icons = "nerd"        # "unicode" when the terminal font has no Nerd Font glyphs
labels = "asdfg"      # keys that select the first visible rows; j, k, l, q and x are reserved
```

Helm checks GitHub for a new release at most once a day. When one exists, the header shows a badge, and `F5` installs it through Herdr. `picker.check_updates = false` turns that off.

## Add your own source

An integration needs a command that lists items as JSON and a command that opens one:

```toml
[[integrations]]
id = "bookmarks"
label = "Bookmarks"
enabled = true
collect = "bookmarks list --json"
open = "bookmarks open {{id}}"
```

`collect` prints an array of `{ "id", "title", "subtitle", "path", "kind" }` objects. Helm shell-quotes each `{{field}}` before running `open`. Integration entries appear in the Other group while you search. [`docs/plugin-integrations.md`](docs/plugin-integrations.md) has the full contract.

## Requirements

- Herdr 0.7.4 or newer, on Linux or macOS
- `jq` for the install link command
- Optional: `zoxide` for directory history, Herdr Plus for Quick Actions and directory templates
- Rust stable, only when building from source

## Build from source

```bash
git clone https://github.com/black-atom-industries/helm.herdr.git
cd helm.herdr
mise run git:config
lefthook install
./scripts/install-local.sh
```

`install-local.sh` builds the release binary, links it to `~/.local/bin/helm-herdr`, and registers the checkout with `herdr plugin link`, so a rebuild takes effect without reinstalling. [`CONTRIBUTING.md`](CONTRIBUTING.md) covers the development workflow.

## Troubleshooting

Check that Herdr sees the plugin:

```bash
herdr plugin list
herdr plugin action list --plugin helm-herdr
```

Print every collected Entry without opening the TUI:

```bash
helm-herdr list
```

When the key does nothing, check that `helm-herdr` is on `PATH` and reload Herdr's config:

```bash
command -v helm-herdr
herdr server reload-config
```

## Docs

- [`docs/index.md`](docs/index.md): architecture, integrations, the plugin contract, and releases
- [`CHANGELOG.md`](CHANGELOG.md): release notes
