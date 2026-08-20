# Open as a live topology tree

Open reflects the current Herdr session as `workspace → tab`.

## Default state

Workspaces are top-level rows and start collapsed. The focused workspace appears first.

```text
 ▾ open  LIVE

   ▸ ◆ [NBR] Notes       ~/repos/nikbrunner/imf-notes
   ▸   [WEB] UI          ~/repos/imfusion/websdk/web-ui
   ▸   [CP] User Portal  ~/repos/imfusion/cp/imfusion-portal

 ▾ agent
   └─ ✓ claude · websdk-211-web-ui-improve…
```

## Expanded workspace

`Enter` focuses a workspace. `Right` exposes its tabs.

```text
   ▸ ◆ [NBR] Notes    ~/repos/nikbrunner/imf-notes
 → ▾   [WEB] UI       ~/repos/imfusion/websdk/web-ui
     ├─ Agents
     ├─ Code
     └─ Server
```

## Search ancestry

A matching tab stays beneath its workspace so repeated names such as `Server`, `Code`, and `Agents` remain distinguishable.

```text
 ▾ open  LIVE

   ▾ [WEB] UI
     └─ Server
   ▾ [CP] User Portal
     └─ Servers
```

## Linked worktrees

Linked-worktree workspaces sit beneath the open non-linked workspace sharing the same repository identity. Each workspace keeps its own tabs. Orphaned worktrees remain top-level.

## Interaction contract

- `Up` / `Down` selects every visible workspace and tab row.
- `Left` / `Right` collapses or expands workspaces.
- `Enter` focuses the selected workspace or tab.
- The active workspace keeps the blue diamond. The active tab gets a green dot.
- Other sources keep their flat result groups below Open.
