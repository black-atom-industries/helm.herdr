# Changelog

All notable changes to this project are documented here.

## [0.7.0](https://github.com/black-atom-industries/helm.herdr/compare/v0.6.0...v0.7.0) (2026-10-27)


### ⚠ BREAKING CHANGES

* **picker:** the topology view with tab and pane lines is replaced, `jump_back.pin_previous` is ignored, and tab results leave search.
* **sources:** the ⌃A agents and ⌃P projects filters are gone, and agent aliases are ignored.
* **sources:** `[sessions] entries` remotes and the ⌃S/⌃L filters are gone. Local session switching is tracked in #3.

### Features

* **keymap:** drop ^B from the footer and close the key sheet with q ([1b88d36](https://github.com/black-atom-industries/helm.herdr/commit/1b88d36238715b3bc0fca226ea8564ded30c9b2c))
* **keymap:** open rows by home-row label and jump by group ([c1166d6](https://github.com/black-atom-industries/helm.herdr/commit/c1166d6bca466947eed5b9af31dfc113a048df93))
* **keymap:** select rows by label, aa opens, xx closes, q quits, l opens the last workspace ([2bd9086](https://github.com/black-atom-industries/helm.herdr/commit/2bd9086958a467d136fd2582ce30b8fbdfddb220))
* **picker:** add a detail pane with agents, changes and recent commits ([fc3a327](https://github.com/black-atom-industries/helm.herdr/commit/fc3a327ce1339eb5c23abcbac963724ea2aea766))
* **picker:** default to Nerd Font status icons ([ecb1d5d](https://github.com/black-atom-industries/helm.herdr/commit/ecb1d5d756bec7f7dace91285711604db0f7afeb))
* **picker:** draw progress bars with ▰▱ like the Claude Code statusline ([91592db](https://github.com/black-atom-industries/helm.herdr/commit/91592db8100da5ad0584264a4339f69c0af0a090))
* **picker:** group workspaces by temperature and send one to Cold with c c ([27689cc](https://github.com/black-atom-industries/helm.herdr/commit/27689cc5ebe29cee5ca92044265e3732f186a4ab))
* **picker:** keep a workspace in Cold only until its next activity ([a38b9b1](https://github.com/black-atom-industries/helm.herdr/commit/a38b9b178bfe0e7abcf6baa023371bd7d8396b41))
* **picker:** label the glance columns and drop the here and last marks ([489f530](https://github.com/black-atom-industries/helm.herdr/commit/489f530315195e2aa542ef2af7a23097b03ba346))
* **picker:** layer the list and detail panels on Herdr's background with tabs on their own line ([fa0702d](https://github.com/black-atom-industries/helm.herdr/commit/fa0702dc03d987a90c902f40ec9ee964ae351140))
* **picker:** lead query rows with the destination ([da8bffa](https://github.com/black-atom-industries/helm.herdr/commit/da8bffaf04c8df80c2e12bd15cf62af1023feb43))
* **picker:** mark the current and previous workspace with a dot after the label ([9462872](https://github.com/black-atom-industries/helm.herdr/commit/94628721f884bf3ea62ff552ce0bde5d3a88264a))
* **picker:** mark the selected glance row with accent bars at both ends ([287f3e3](https://github.com/black-atom-industries/helm.herdr/commit/287f3e34cebb632f2e6c881e0976ffa8128a0e15))
* **picker:** open on the glance, one row per workspace grouped by warmth ([0fc0d79](https://github.com/black-atom-industries/helm.herdr/commit/0fc0d79062aa130f1b93779838ccff8cee8f5523))
* **picker:** order footer hints by navigation and pad the view evenly ([d8fd763](https://github.com/black-atom-industries/helm.herdr/commit/d8fd763c54913d815d3d4a71c79bd29338201050))
* **picker:** put the cursor on the best search match and drop weak matches ([c4fe2b3](https://github.com/black-atom-industries/helm.herdr/commit/c4fe2b3cbe29af2c25ad6a2106e6dd0219ba2842))
* **picker:** separate list, frame and bars by background instead of lines ([4a0bda6](https://github.com/black-atom-industries/helm.herdr/commit/4a0bda6ba92b30bc37756becb3536887e1fbcea7))
* **picker:** show a legend for the row marks and agent icons ([262748d](https://github.com/black-atom-industries/helm.herdr/commit/262748d2002604b9407f498fec2e88fb4c7572ef))
* **picker:** show a size notice below 120 × 32 instead of a squeezed layout ([236c7dc](https://github.com/black-atom-industries/helm.herdr/commit/236c7dc2395be4adb4cc7a8b8abb626214d222fc))
* **sessions:** add helm-herdr shell, a launcher that re-attaches Herdr sessions ([1929516](https://github.com/black-atom-industries/helm.herdr/commit/1929516a8914aff318b5a499e522062a8ff335b7))
* **sessions:** add helm-herdr status for Herdr's tab bar ([3203f53](https://github.com/black-atom-industries/helm.herdr/commit/3203f53344606a401e5a0fa271fcc241260bb85b))
* **sessions:** count an agent as done when it finishes after you left its session ([2dc3cb8](https://github.com/black-atom-industries/helm.herdr/commit/2dc3cb8051bdfbac8fc9a301301a248abae387d9))
* **sessions:** list every Herdr session in a ^S sessions view ([a9ef287](https://github.com/black-atom-industries/helm.herdr/commit/a9ef28744003c4c70c58e6ed135ea6da1351a1b6))
* **sessions:** show agents waiting in other Herdr sessions in Needs you ([3f10889](https://github.com/black-atom-industries/helm.herdr/commit/3f1088984e1e1693df43fe6f05cf599629a874ff))
* **sessions:** switch to another Herdr session from Helm ([79e32eb](https://github.com/black-atom-industries/helm.herdr/commit/79e32eb39475fa07577f9c6c135d7bd0df22990f))
* **sources:** drop the agents and projects sources ([9a46e60](https://github.com/black-atom-industries/helm.herdr/commit/9a46e6068f0cb90c96955e890838c9ced746dac8))
* **sources:** drop the remote server and session sources ([3708356](https://github.com/black-atom-industries/helm.herdr/commit/3708356c0080f4c02ca40a5d7ee520f3179dfb9c))
* **theme:** add popup background and retune chrome tones ([5d7aaae](https://github.com/black-atom-industries/helm.herdr/commit/5d7aaaeb9bef0d3577fff2ccdd7eca0f8f2c4cce))
* **topology:** record focus, agent state and last commit times ([8ed271a](https://github.com/black-atom-industries/helm.herdr/commit/8ed271a63a29a22a820629ca27961d72c66da52b))


### Bug Fixes

* **keymap:** fit the key sheet in the popup with two columns and tight key highlights ([c7e62cb](https://github.com/black-atom-industries/helm.herdr/commit/c7e62cb0a3d37727e0367ea2a9f70cd383e2339f))
* **picker:** give the detail pane the same vertical padding as the list ([ddc8307](https://github.com/black-atom-industries/helm.herdr/commit/ddc8307df3c9d07611d88e86ca2e79a7456a9104))
* **picker:** keep activity per session and rows steady while open ([951369b](https://github.com/black-atom-industries/helm.herdr/commit/951369b433edc243553a76889c2deb20a5a00858))
* **picker:** keep agent and path keys out of fuzzy matching ([a259bb2](https://github.com/black-atom-industries/helm.herdr/commit/a259bb2101497910e8b95d944c475019d1f1fa9f))
* **picker:** name the current Herdr session on its own line with an icon ([6f302c0](https://github.com/black-atom-industries/helm.herdr/commit/6f302c0e15cf305160f3479174f09cdbeceaaa50))
* **picker:** open with the current workspace selected ([598de9d](https://github.com/black-atom-industries/helm.herdr/commit/598de9de2cbdf93b3d08cdc09bb44cc100d9c1cd))
* **sessions:** report failed switches and keep launcher state in one folder ([7c508af](https://github.com/black-atom-industries/helm.herdr/commit/7c508afb469cc6486312469aaf7368da58b4dabf))
* **theme:** tint the selected row from the text scale ([1b8c0de](https://github.com/black-atom-industries/helm.herdr/commit/1b8c0def6cb214618ef357d0347417f9c03ef0f6))


### Documentation

* describe the glance, the detail pane and the keys ([0a3ae33](https://github.com/black-atom-industries/helm.herdr/commit/0a3ae333fc21dbc61f8773548fa9008ef50ff482))
* name c among the reserved glance label keys ([f3ab739](https://github.com/black-atom-industries/helm.herdr/commit/f3ab73995f57772049bdad47e475020698b908a7))
* rewrite the README around the topology and query views ([e605685](https://github.com/black-atom-industries/helm.herdr/commit/e6056851177c40c36a736dc21e264e38c2839b2e))

## [0.6.0](https://github.com/black-atom-industries/helm.herdr/compare/v0.5.0...v0.6.0) (2026-10-18)

### Highlights

#### Live topology picker

Helm opens on the current session as Workspace → Tab → Pane blocks. `h` `j` `k` `l` walk the levels, `J` and `K` jump between Workspaces from any depth, and Enter focuses the exact Pane.

![Walking from the api Workspace into web's preview Tab and focusing the vite Pane](https://raw.githubusercontent.com/black-atom-industries/helm.herdr/v0.6.0/docs/assets/changelog/0.6.0-topology.webp)

#### One Query for open and not-yet-open

Typing turns the topology into flat one-line rows: open Workspaces, Tabs, Panes, agents, zoxide directories, and configured roots. Enter focuses what is open and creates a Workspace for what is not.

![Typing "do" mixes the docs Workspace with zoxide and root directories, then "infra" opens a new Workspace from a root](https://raw.githubusercontent.com/black-atom-industries/helm.herdr/v0.6.0/docs/assets/changelog/0.6.0-query.webp)

#### Herdr owns the popup

`helm-herdr` runs directly in a Herdr popup binding, so the key and the size are yours:

```toml
[[keys.command]]
key = "prefix+t"
type = "popup"
command = "helm-herdr"
width = "90%"
height = "90%"
```

Colors follow the terminal and Herdr's themes, and auto-switching themes track the macOS appearance. Working agents get an animated indicator.

#### Important fixes

- Jump-back follows every Workspace focus change, including Herdr's own `previous_agent` and `next_agent` keys.
- The macOS appearance probe stays out of the popup, where it used to flash `Dark`.

#### Upgrading from 0.5

The plugin is now `helm-herdr` from `black-atom-industries/helm.herdr`. Install it under the new name; on first run Helm copies the config from `herdr-navigator`.

```diff
-herdr plugin install thanhdat77/herdr-navigator --yes
+herdr plugin install black-atom-industries/helm.herdr --yes
```

Point the binding at the new binary:

```diff
-type = "plugin_action"
-command = "herdr-navigator.open"
+type = "popup"
+command = "helm-herdr"
```


### chore

* release 0.6.0 ([61eeb86](https://github.com/black-atom-industries/helm.herdr/commit/61eeb86b9bba7df582fe7d8aefa2e7a46ee1afe8))


### Bug Fixes

* **theme:** keep the appearance probe out of the popup ([5e4e5b1](https://github.com/black-atom-industries/helm.herdr/commit/5e4e5b119e59c4946aba9f0cfff73051af749ccf))


### Documentation

* add the 0.6.0 topology and query clips with their tapes ([bd59342](https://github.com/black-atom-industries/helm.herdr/commit/bd593426a598dfacf62ecb4f8aeba128900ddbe6))

## [0.5.0] - 2026-08-07

### Changed
- **Breaking:** New project and directory workspaces use plain names instead of `project:` and `dir:` prefixes.

## [0.4.0] - 2026-08-07

### Changed
- **Breaking:** Local Herdr sessions are not listed because Navigator cannot switch the terminal tab that owns them.

### Fixed
- Agent terminal IDs remain searchable while focus actions continue to target pane IDs.

## [0.3.5] - 2026-08-02

### Added
- Mouse wheel navigation, click-to-select, and click-again-to-open support in the TUI.
- `Ctrl-Backspace` deletes the last word of the query.

### Fixed
- Mouse hitboxes now start at the list's inner content row, accounting for the `Results` title instead of selecting the result below the cursor.
- Unbound `Ctrl`/`Alt` chords no longer insert their letter into the query; `Ctrl-Backspace` typed an `h` because terminals send it as `Ctrl-H`.
- Equal-score results now keep their source order (zoxide frecency, agent pane order) instead of sorting alphabetically.

## [0.3.4] - 2026-07-29

### Added
- Apply one reusable Herdr Plus tabs/panes template to any zoxide/root directory with a configurable shortcut (`Alt-Enter` by default), creating the workspace or appending fresh template tabs when already open; `Enter` keeps normal behavior.
- Press `F5` on the update badge to confirm and install the available release through Herdr.

### Fixed
- Focus agent rows by Herdr pane ID, which `herdr agent focus` accepts, instead of unsupported terminal IDs ([#13](https://github.com/thanhdat77/herdr-navigator/issues/13), [#18](https://github.com/thanhdat77/herdr-navigator/issues/18)).

## [0.3.3] - 2026-07-18

### Fixed
- Inherit all 18 built-in themes from Herdr 0.7.4, including Dracula ([#3](https://github.com/thanhdat77/herdr-navigator/issues/3)).
- Preserve Herdr Plus `[[tabs.panes]]` splits, labels, and startup commands when opening projects from Navigator.

## [0.3.2] - 2026-07-15

### Added
- Notification audio modes: Herdr defaults, silent notifications, or an asynchronous custom audio file.
- A new full-width Navigator demo replaces the cropped preview.

### Changed
- **Breaking:** the plugin id, Cargo package/binary, action prefix, config directory, and release archives are now consistently named `herdr-navigator`. On first run, missing files are copied from the legacy `herdr-picker-plus` config directory without deleting the originals.

### Fixed
- Navigator notifications can now be disabled completely with `[notifications] enabled = false` ([#1](https://github.com/thanhdat77/herdr-navigator/issues/1)).

## [0.3.1] - 2026-07-15

### Added
- Non-blocking daily GitHub release checks show an `↑ vX.Y.Z available` header badge; `picker.check_updates = false` disables them and network failures stay silent.

## [0.3.0] - 2026-07-15

### Added
- Herdr-style source-aware result rows keep status/tab/pane metadata in a right column and expand only zoxide/root entries to a second full-path line; native `prefix+g` glyphs mark selection, focus, blocked, working, idle, done, and unknown states. `picker.detailed_rows = false` restores the compact list.
- Re-invoking the overlay `open` action focuses the existing Navigator in the current workspace instead of opening a duplicate pane.
- Herdr Navigator banner, social preview, and a shorter outcome-led README.
- Configurable Jump Back action (`herdr-picker-plus.jump-back`) toggles to the workspace left by the last successful local picker navigation and can pin that workspace first in the initial picker view.
- Persistent side pane mode: the `open-side` action opens the picker in a right split (like herdr-file-viewer). Launch-or-focus, toggles closed when already focused, and the picker stays open after `Enter`.
- `Ctrl-X` closes the selected/open matching workspace without closing the picker; the picker refuses to close its owning workspace.
- Built-in server/remote source from remote `[sessions.entries]`, using `herdr --remote TARGET --handoff`, plus local session entries from `herdr session list --json`.

### Changed
- User-facing name and GitHub repository are now Herdr Navigator; the stable `herdr-picker-plus` plugin id, binary, config path, and action prefix are unchanged.
- Agent source now uses the dedicated `herdr agent list` endpoint instead of filtering `herdr pane list`, and agents are searchable by their `agent_session` id.
- Workspace rows surface the workspace-level `agent_status` from `herdr workspace list` in the subtitle, and match `focused`/status search terms.
- Requires Herdr 0.7.3+ (`min_herdr_version` bumped for `agent list` and workspace `agent_status`).
- Empty default picker results now honor agent status priority when `agent_sort = "priority"` or Herdr `agent_panel_sort = "priority"` is active.
- Agent status priority is now: blocked/error/fail, attention/request/wait, done/complete, working/running, idle/unknown.
- Removed built-in SSH/server-terminal handling from Picker; `Ctrl-S` now hands off to Herdr remote servers.
- Picker footer uses compact Ctrl-style key hints.

## [0.2.0] - 2026-06-29

### Added
- Command/JSON plugin integration contract via `[[integrations]]`.
- Herdr success/error notifications for selected actions.
- Agent search by agent name, workspace/session label, cwd/path, status, ids, and aliases.
- Agent shortcut: `@` as a Ctrl-A-style full agent view with configurable sorting.
- Server source from `~/.ssh/config` and manual `[[servers.entries]]`, with `Ctrl-S` filtering and SSH connect inside a server workspace tab. This moved to the separate `herdr-server-aware` plugin after 0.2.0.

### Changed

- Agent rows stay tied to the pane start directory while still searching current foreground cwd.
- Herdr Plus logic now lives in a built-in integration adapter.
- Picker entries dispatch through actions instead of hardcoding behavior by source.

## [0.1.2] - 2026-06-28

### Fixed
- Show multiple open workspaces with the same cwd instead of deduping by path.
- Reuse project and directory workspaces by source kind so project and zoxide/root entries do not steal each other.


## [0.1.1] - 2026-06-28

### Added
- Herdr Plus Quick Actions launcher entry.

## [0.1.0] - 2026-06-28

### Added
- Herdr overlay picker plus.
- Sources for open workspaces, Herdr Plus projects, zoxide, configured roots, and agents.
- Configurable source order and search engine.
- Herdr theme-name inheritance with custom token overrides.
- Release and CI workflows for public builds.
