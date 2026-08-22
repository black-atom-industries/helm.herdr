# Helm for Herdr

> [!NOTE]
> Part of the [Helm](https://github.com/black-atom-industries) family. See also [helm.tmux](https://github.com/black-atom-industries/helm.tmux) for the tmux version.

> [!NOTE]
> Originally forked from [thanhdat77/herdr-navigator](https://github.com/thanhdat77/herdr-navigator). Thanks to thanhdat77 for the foundation.

<p align="center">
  <strong>One fuzzy navigator for every workspace, agent, project, session, remote, directory, and action in Herdr.</strong>
</p>

<p align="center">
  <a href="https://github.com/black-atom-industries/helm.herdr/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/black-atom-industries/helm.herdr/actions/workflows/ci.yml/badge.svg" /></a>
  <a href="LICENSE"><img alt="MIT License" src="https://img.shields.io/badge/license-MIT-2ea44f" /></a>
  <img alt="Herdr 0.7.4+" src="https://img.shields.io/badge/Herdr-0.7.4%2B-66b3ff" />
  <img alt="Linux and macOS" src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-c084fc" />
</p>

Type what you remember. Helm decides whether to **focus, create, attach, hand off, invoke, or run**—without making you remember which Herdr surface owns the destination.

```text
prefix+t  →  type  →  Enter
```

## Install

```bash
herdr plugin install black-atom-industries/helm.herdr --yes
herdr plugin action invoke helm-herdr.open
```

The action opens Helm in a session-modal Herdr popup. Add a shortcut to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+t"
type = "plugin_action"
command = "helm-herdr.open"
description = "jump to anything"
```

A single result list can move between live Herdr state and things that are not open yet:

- Type a repo name → focus its open workspace, or create one from a project, zoxide, or configured root.
- Type `@idle` or an agent alias → focus that agent pane.
- Filter remotes → hand off with Herdr's own `--remote TARGET --handoff` flow.
- Select an external integration → run its configured action.

## Why Helm

| Capability                  | Why it matters                                                                                                                        |
| --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| **One index across Herdr**  | Search workspaces, agents, projects, sessions, remotes, directories, Quick Actions, and integrations together.                        |
| **Action-aware Enter**      | Results do not just return paths; they focus, create, attach, hand off, invoke, or run.                                               |
| **Reuse first**             | Existing workspaces are focused before new ones are created. Project and directory workspaces sharing a cwd keep separate identities. |
| **Agents are first-class**  | Search agent name, status, workspace, cwd, pane/tab/terminal IDs, session ID, and your own aliases.                                   |
| **Extensible without Rust** | Add another tool with a command that returns JSON and a command that opens the selected item.                                         |
| **No picker dependency**    | The Rust/ratatui interface runs in a Herdr-managed pane; `fzf` and `tv` are not runtime requirements.                                 |

Herdr's built-in navigation remains the simpler choice for a single entity type. Helm is for the moment when "where next?" could mean a workspace, agent, path, session, remote, project, or action.

## Standout features

| Feature                | What it does                                                                                                                                                                        |
| ---------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Last Workspace**     | `helm-herdr.jump-back` toggles between the current and previously visited local workspace.                                                                                          |
| **Pin**                | `Ctrl-B` marks important entries and keeps them ahead of the normal source order.                                                                                                   |
| **Open with Template** | `Alt-Enter` by default applies one reusable Herdr Plus tabs/panes template to any zoxide/root directory, creating its workspace or appending fresh template tabs when already open. |

## What it can open

| Source    | Data                              | Enter does                                                         |
| --------- | --------------------------------- | ------------------------------------------------------------------ |
| `open`    | Current-session workspaces → tabs | Focus the exact workspace or tab                                   |
| `agent`   | `herdr agent list`                | Focus the agent pane                                               |
| `project` | Herdr Plus project TOML           | Reuse or create a project workspace and apply tabs and split panes |
| `server`  | Configured remote targets         | Hand off to the remote Herdr server                                |
| `zoxide`  | `zoxide query -l`                 | Enter opens normally; `Alt-Enter` applies the shared template      |
| `root`    | Configured filesystem roots       | Enter opens normally; `Alt-Enter` applies the shared template      |
| `quick`   | Herdr Plus Quick Actions          | Open the Quick Actions picker                                      |
| `plugin`  | Command/JSON integrations         | Run the configured open command                                    |

Every source can be disabled. Missing optional tools degrade quietly.

## Keyboard workflow

| Key              | Action                                                                  |
| ---------------- | ----------------------------------------------------------------------- |
| type             | Fuzzy search                                                            |
| `Enter`          | Open selected item normally                                             |
| `Alt-Enter`      | Apply `picker.directory_template` to the selected zoxide/root directory |
| `Up` / `Down`    | Move across visible workspace, tab, and flat-source rows                |
| `Left` / `Right` | Collapse/expand Open workspaces                                         |
| `Ctrl-W`         | Open topology (workspaces and tabs)                                     |
| `Ctrl-A`         | Agents, using configured status order                                   |
| `Ctrl-P`         | Herdr Plus projects                                                     |
| `Ctrl-Q`         | Herdr Plus Quick Actions                                                |
| `Ctrl-S`         | Remotes                                                                 |
| `Ctrl-L`         | Sessions                                                                |
| `Ctrl-Z`         | Zoxide                                                                  |
| `Ctrl-R`         | Roots                                                                   |
| `Ctrl-X`         | Close the open workspace matching the selected item                     |
| `Ctrl-B`         | Mark or unmark the selected item                                        |
| `Ctrl-U`         | Clear query and filter                                                  |
| `Ctrl-Backspace` | Delete the last query word                                              |
| `?`              | Show active keybindings                                                 |
| `Esc` / `Ctrl-C` | Back or close                                                           |

Open is a live topology for the current session: workspaces are top-level rows and contain tabs. Linked-worktree workspaces are nested beneath the open non-linked workspace for the same repository, with their own tabs one level deeper; orphaned worktrees remain top-level. Workspaces start collapsed, with the focused workspace selected; `Left`/`Right` collapse or expand them. Search retains matching tabs' workspace and linked-worktree ancestry so repeated names such as `Code`, `Agents`, or `Server` remain distinguishable.

Status glyphs follow Herdr's `prefix+g` visual language. Agent rows use `◉` for blocked/attention, an animated Braille spinner for working, `●` for done, `✓` for idle, and `○` for unknown. Workspace status dots use `●` for blocked/working/done, `○` for idle, and `·` for unknown. Diamond color priority is marked yellow, current accent/blue, then previous red. The active tab uses a green dot. Selection uses `→`, and topology/source trees use `▾`, `▸`, `├─`, and `└─` markers.

Within flat sources, the previous workspace stays first on the initial unfiltered view, followed by marked items and then the normal source order.

Structured search narrows large result sets:

```text
!claude          # agent name
@idle            # agent workspace/status
@Dotfiles        # agent workspace label or id
/dotfiles        # cwd/path
```

In Normal mode, `h`/`j`/`k`/`l` mirror the arrow navigation keys. Query editing keeps those characters as text. Source shortcuts can be remapped through `[picker.filter_keys]`.

## Power moves

### Close an open directory

Select an `open` or `agent` entry, or a `project`, `root`, or `zoxide` entry that matches an open workspace, then press `Ctrl-X`. Helm closes that workspace and refreshes the list.

Helm refuses to close the workspace that owns the picker; switch away first. Directories that are not open and server, session, quick-action, or plugin entries are left unchanged.

### Jump Back

Helm remembers the workspace left by a successful local navigation. Bind the dedicated action for tmux-style current/previous toggling:

```toml
[[keys.command]]
key = "prefix+l"
type = "plugin_action"
command = "helm-herdr.jump-back"
description = "jump to previous workspace"
```

The previous workspace can also stay pinned at the top of the initial picker view:

```toml
[jump_back]
# Record local workspace transitions and enable the action.
enabled = true
# Pin the previous workspace only while the picker is unfiltered.
pin_previous = true
```

If the previous workspace was closed, the next Jump Back clears the stale state and reports it.

### Persistent side pane

Keep Helm beside your work:

```bash
herdr plugin action invoke helm-herdr.open-side
```

The action opens the side pane, focuses it when it already exists, and closes it when invoked while focused. Unlike the popup, the side pane stays open after `Enter`.

Optional binding:

```toml
[[keys.command]]
key = "prefix+shift+t"
type = "plugin_action"
command = "helm-herdr.open-side"
description = "helm side pane"
```

## Configuration

Helm writes a fully commented config on first run:

```bash
herdr plugin config-dir helm-herdr
```

See [`examples/default-config.toml`](examples/default-config.toml) for every option and its behavior. Common customizations:

```toml
[picker]
reuse_existing = true
create_missing = true
engine = "nucleo" # nucleo | skim | simple
source_order = ["workspace", "agent", "project", "zoxide", "root", "server", "quick", "plugin"]
source_priority_boost = 5
agent_sort = "herdr" # herdr | priority | spaces
popup_width = 90
popup_height = 90
check_updates = true # daily background release check
# directory_template = "default.toml" # Herdr Plus project file
# directory_template_key = "alt-enter" # or ctrl-g / ctrl-t
[notifications]
enabled = true
audio = false # set true to enable sound
sound = "default" # default | custom
custom_sound = "" # Example: "~/sounds/navigator.wav"

[sources]
open_workspaces = true
agents = true
herdr_plus_projects = true
herdr_plus_quick_actions = true
sessions = false
servers = true
zoxide = true
roots = true

[[roots]]
path = "~/workspace"
max_depth = 3
```

Useful config surfaces:

- `picker.popup_width` and `picker.popup_height` set the session-modal popup size as integer percentages from 1 through 100.
- `picker.check_updates` checks GitHub releases in the background at most daily and shows `↑ vX.Y.Z available · F5 update`; press `F5`, confirm, and Helm installs that release through Herdr. Failures stay silent until an update is requested.
- `picker.directory_template = "default.toml"` reuses that Herdr Plus project file from its `projects/` config directory. `Enter` keeps normal reuse/create behavior. `picker.directory_template_key` defaults to `alt-enter` and also accepts Ctrl forms such as `ctrl-g`; the shortcut always applies all template tabs, panes, labels, and commands using the selected directory instead of the template's `working_dir`, creating the workspace or appending fresh template tabs.
- `[notifications]` can disable notifications entirely or use Herdr's default sounds, no sound, or a custom audio file.
- `[picker.filter_keys]` remaps source shortcuts.
- `[[agent_aliases]]` adds memorable search terms without renaming Herdr panes.
- `[[sessions.entries]]` configures remote targets.
- `[theme]` inherits supported Herdr themes and custom tokens.
- `[[integrations]]` adds external command/JSON sources.

## Add your own source

A tool only needs a list command and an open command:

```toml
[[integrations]]
id = "bookmarks"
label = "Bookmarks"
enabled = true
collect = "bookmarks list --json"
open = "bookmarks open {{id}}"
notify_success = true
notify_error = true
```

`collect` prints a JSON array:

```json
[
  {
    "id": "abc",
    "title": "Item",
    "subtitle": "Info",
    "path": "/tmp",
    "kind": "bookmark"
  }
]
```

Helm shell-quotes `{{id}}`, `{{title}}`, `{{subtitle}}`, `{{path}}`, and `{{kind}}` before running `open`. See [`docs/plugin-integrations.md`](docs/plugin-integrations.md) for the full contract.

## Requirements

- Herdr `0.7.4` or newer
- Linux or macOS
- Optional: `zoxide` for directory history
- Optional: Herdr Plus for project templates and Quick Actions
- Rust stable + Cargo only when building from source

Build and link locally:

```bash
git clone https://github.com/black-atom-industries/helm.herdr.git
cd helm.herdr
cargo build --release
herdr plugin link "$PWD"
```

## Troubleshooting

Check that Herdr sees the plugin and its actions:

```bash
herdr plugin list
herdr plugin action list --plugin helm-herdr
```

Inspect every collected candidate without opening the TUI:

```bash
./target/release/helm-herdr list
```

If a keybinding does nothing, verify the action ID and reload config:

```bash
rg "helm-herdr.open" ~/.config/herdr/config.toml
herdr server reload-config
```

Optional sources can be checked independently:

```bash
zoxide query -l
find ~/.config/herdr/plugins/config/cloudmanic.herdr-plus/projects -name '*.toml'
```

## Project docs

- [`ROADMAP.md`](ROADMAP.md) — planned todos
- [`docs/architecture.md`](docs/architecture.md) — runtime flow and design
- [`docs/integrations.md`](docs/integrations.md) — Herdr/plugin integration patterns
- [`docs/plugin-integrations.md`](docs/plugin-integrations.md) — command/JSON contract
- [`CHANGELOG.md`](CHANGELOG.md) — released and unreleased changes
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — development workflow

Helm for Herdr is intentionally small: reuse Herdr primitives, keep optional integrations optional, and make the common path `prefix+t → type → Enter`.
