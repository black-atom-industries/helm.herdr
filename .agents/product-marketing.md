# Product Marketing Context

*Last updated: 2026-10-25*

## Product Overview

**Display name:** Helm for Herdr
**Technical plugin ID:** `helm-herdr`
**One-liner:** One glance at every Herdr workspace: what needs you, what is working, and one key to get there.
**What it does:** Helm opens on the glance: one row per open workspace, grouped by warmth (Pinned, Needs you, Hot, Warm, Cold) from agent state, last focus and last commit. Home-row labels open the top rows in one key, and a detail pane shows tabs, agents with subagent progress, uncommitted changes and recent commits. Typing searches the same list plus roots, zoxide, Quick Actions and command integrations; Enter performs the right Herdr action: focus, create, invoke, or run.
**Product category:** Herdr plugin; workspace overview and switcher; terminal fuzzy navigator.
**Product type:** Free, MIT-licensed open-source developer tool.

## Target Audience

**Primary users:** Keyboard-first Herdr users managing several repositories, workspaces, and AI-agent panes.
**Primary use case:** See at a glance which workspace or agent needs attention, and get there in one keystroke.
**Jobs to be done:**
- See which agents wait on me, which are working, and which workspaces went quiet.
- Jump to a workspace, or straight to one of its tabs, in one or two keys.
- Open a directory without creating duplicate workspaces.
- Bring another CLI tool into the same picker without changing Rust code.

**Anti-persona:** Users who do not run Herdr, only need a basic path picker, or want process/session restoration.

## Problems & Pain Points

**Core problem:** With several agents running across workspaces, the state of the work is scattered: who waits for a reply, who is done, what changed where. Herdr's tab bar and workspace list show names, not urgency.
**Why alternatives fall short:** Herdr's focused pickers are narrower; generic fuzzy finders return paths but do not know whether to focus, create, attach, or hand off; custom scripts become fragmented and hard to share.
**Emotional tension:** Context switching breaks flow, and a waiting agent nobody noticed costs time.

## Competitive Landscape

**Direct:** Herdr's built-in workspace navigation is simpler for one entity type, but not a cross-source action surface.
**Secondary:** `fzf`, `tv`, zoxide, and custom shell scripts — flexible discovery, but no built-in Herdr action semantics or source-aware workspace reuse.
**Analogues:** tmux-sessionx and tmux-fzf — validate the unified switch-or-create workflow, but target tmux rather than Herdr.

## Differentiation

- Workspaces grouped by what they need from you, not by name or kind.
- Home-row labels and tab numbers: one or two keys to any open place.
- A detail pane with agent and subagent progress, changes and recent commits, without leaving the popup.
- One searchable index across workspaces, roots, zoxide, and actions.
- Enter performs the source-appropriate Herdr operation instead of printing a path.
- Reuse-first and source-aware identity avoid duplicate or incorrect workspace focus.
- Agent status, workspace, cwd, and IDs are searchable.
- External tools integrate through a small command/JSON `collect` + `open` contract.
- Built-in Rust/ratatui interface; no external picker UI dependency.

## Objections

| Objection | Response |
|-----------|----------|
| Herdr already has navigation | Helm adds urgency: grouping by agent state and recent activity, plus progress and git state per workspace. |
| I do not use every integration | Every source can be disabled; Herdr Plus and zoxide degrade quietly when absent. |
| The v0.3.2 technical rename disrupts existing installs | Keep the one-time migration explicit; old config and Jump Back state copy automatically on first run. |
| Will it create duplicate workspaces? | It focuses matching open workspaces first and distinguishes project workspaces from plain directory workspaces sharing the same cwd. |

## Customer Language

**Inferred problem language:**
- “I know where I want to work; just take me there.”
- “Which agent is waiting on me right now?”
- “I do not want a separate picker for workspaces and directories.”
- “Focus what is already open before creating another workspace.”

**Words to use:** glance, needs you, jump, focus, reuse, create, workspace, agent, one key, type what you remember.
**Words to avoid:** blazing fast, zero friction, native custom UI, SSH server picker, AI-powered, revolutionary.

## Brand Voice

**Tone:** Direct, technical, calm, useful.
**Style:** Outcome first, exact commands second, implementation detail later.
**Personality:** Focused, capable, keyboard-first, honest, extensible.

## Proof Points

- MIT-licensed Rust project with Linux and macOS declared support.
- Herdr 0.7.4+ actions and event hooks for workspace focus/create, tab focus, agent state tracking, and managed plugin panes.
- Configurable glance windows, row labels, Nerd Font or Unicode icons, source order, themes, fuzzy engines, roots, Jump Back, and command integrations.
- Automated Rust tests and CI/release workflows.
- Clips are recorded from the VHS tapes in `docs/assets/tapes/`.

No benchmarks, testimonials, adoption claims, or quantified time savings are currently available.

## Goals

**Primary goal:** Increase qualified Herdr-user installs from the GitHub README.
**Primary conversion action:** Run `herdr plugin install black-atom-industries/helm.herdr --yes`.
**Secondary actions:** Star the repository, bind the popup key, and contribute integrations.
