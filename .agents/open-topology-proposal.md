# Open as a live topology tree

> Reflect Herdr's actual `session → workspace → tab` structure in Helm instead of presenting open workspaces as another flat source.

## Default state

The current session and current workspace start expanded. Other workspaces and sessions stay collapsed.

```text
 Search  │
┌─ Results ─────────────────────────────────────────────────────────────────────────────┬─ Preview ──────────────────────────┐
│                                                                                      │                                   │
│ ▾ open  LIVE                                                                         │  TAB                              │
│                                                                                      │  notes                            │
│   ▾ imfusion                                           session · 13 workspaces       │                                   │
│     ├─ ▾ ◆ [NBR] Notes    ~/repos/nikbrunner/imf-notes workspace · 3 tabs            │  session     imfusion             │
│     │   ├─   agents                                tab · 1 pane                       │  workspace   [NBR] Notes           │
│ →   │   ├─ ● notes                                 tab · 1 pane                       │  tab         notes · 1 pane        │
│     │   └─   3                                     tab · 1 pane                       │                                   │
│     ├─ ▸   [WEB] UI       ~/repos/imfusion/websdk/web-ui          workspace · 3 tabs  │  Enter focuses this exact tab      │
│     ├─ ▸   [CP] User Portal  ~/repos/imfusion/cp/imfusion-portal  workspace · 4 tabs  │                                   │
│     └─ ▸   10 more workspaces                                                         │                                   │
│                                                                                      │                                   │
│   ▸ nikbrunner                                         session · 4 workspaces        │                                   │
│                                                                                      │                                   │
│ ▾ agent                                                                              │                                   │
│   └─ ✓ claude · websdk-211-web-ui-improve…                                               │                                   │
├──────────────────────────────────────────────────────────────────────────────────────┴───────────────────────────────────┤
│ ↑↓ navigate · ←→ collapse/expand · ↵ focus                                  17 workspaces · 45 tabs                   │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

## Expanding another workspace

Workspace rows remain useful targets. `Enter` focuses the workspace; `Right` exposes its tabs.

```text
│   ▾ imfusion                                           session · 13 workspaces       │
│     ├─ ▸ ◆ [NBR] Notes    ~/repos/nikbrunner/imf-notes workspace · 3 tabs            │
│ →   ├─ ▾   [WEB] UI       ~/repos/imfusion/websdk/web-ui          workspace · 3 tabs  │
│     │   ├─   Agents                                tab · 2 panes                      │
│     │   ├─   Code                                  tab · 1 pane                       │
│     │   └─   Server                                tab · 2 panes                      │
│     ├─ ▸   [CP] User Portal  ~/repos/imfusion/cp/imfusion-portal  workspace · 4 tabs  │
│     └─ ▸   10 more workspaces                                                         │
│                                                                                      │
│   ▸ nikbrunner                                         session · 4 workspaces        │
```

## Search keeps ancestry

A matching tab stays under its session and workspace. Search does not flatten away the context needed to distinguish repeated names such as `Server`, `Code`, or `Agents`.

```text
 Search  server│

 ▾ open  LIVE

   ▾ imfusion                                              session · 3 matches
     ├─ ▾ [WEB] UI                ~/repos/imfusion/websdk/web-ui
 →   │   └─ Server                                      tab · 2 panes
     ├─ ▾ [CP] User Portal        ~/repos/imfusion/cp/imfusion-portal
     │   └─ Servers                                     tab · 1 pane
     └─ ▾ imfusion-portal-CP-89   ~/repos/imfusion/cp/imfusion-portal-CP-89
         └─ Servers                                     tab · 2 panes

 ▾ agent
   └─ server-monitor                                      score 61
```

## Linked worktree nesting

Linked-worktree workspaces sit beneath the open non-linked workspace sharing the same repository identity. Each workspace keeps its own tabs; orphaned worktrees remain top-level.

```text
   ▾ [WEB] UI
     ├─ Agents
     ├─ Code
     ├─ Server
     ├─ ▸ websdk-169-storybook-controls
     ├─ ▾ websdk-211-web-ui-improve-consumer-llm-hook-flow-a
     │   ├─ Agents
     │   ├─ Code
     │   └─ Servers
     └─ ▸ websdk-182-web-ui-theme-the-storybook-chrome-with
```

## Interaction contract

- `Up` / `Down` selects every visible session, workspace, and tab row.
- `Left` / `Right` collapses or expands sessions and workspaces.
- `Enter` focuses the selected session, workspace, or tab.
- The active workspace keeps the blue diamond. The active tab gets a green dot.
- Other sources keep their current flat result groups below Open.
