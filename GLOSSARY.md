# Ubiquitous Language

## Agent workflow

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **User** | The person who directs the agent and makes decisions. | Human, operator |
| **Agent** | The coding assistant that performs work in the repository. | Bot, assistant |
| **Skill** | A prescribed workflow for handling a class of tasks. | Prompt, recipe |
| **Instruction** | A rule that governs how the agent behaves or how the project is handled. | Guideline, suggestion |
| **Glossary** | The canonical vocabulary used to describe the project domain. | Terminology list, word list |

## Herdr packaging

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Herdr** | The host application that loads and runs plugins. | Runner, host tool |
| **Herdr plugin** | A plugin package that Herdr can load as a named executable. | Extension, add-on |
| **Local plugin** | A Herdr plugin linked to an executable built from the current repository. | Development install, local build |

## Picker interface

### Picker controls

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Picker** | Helm's interactive interface for finding and opening destinations. | Navigator, menu |
| **Popup** | A temporary Picker shown by Herdr over the current workspace. | Modal |
| **Side pane** | A persistent Picker shown beside the current workspace. | Split view |
| **Query** | The text entered by the User to narrow the result list. | Search term |
| **Source filter** | A restriction that limits the result list to one Source category. | Filter |
| **Result list** | The ordered visible collection of Entries matching the Query and Source filter. | Results, menu items |
| **Help** | The panel listing the active Keybindings. | Help screen, shortcuts menu |
| **Keybinding** | A key or key combination mapped to a Picker command. | Shortcut, hotkey |

### Entries

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Entry** | A navigable destination represented in the Result list. | Result, item |
| **Entry row** | The one-line rendering of an Entry as `Source | symbol+word status | destination`. | Result row, list item |
| **Selected entry** | The Entry currently targeted by keyboard or mouse actions. | Highlighted row, active item |
| **Marked entry** | An Entry explicitly kept ahead of normal source ordering. | Pinned item, favorite |
| **Entry action** | The operation performed when the User opens the Selected entry. | Result behavior, command |

## Navigation model

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Workspace** | A Herdr container for related Tabs and their Panes. | Project, session |
| **Tab** | A named view within a Workspace containing one or more Panes. | Workspace tab, window |
| **Pane** | A terminal area within a Tab. | Panel, split |
| **Topology** | The live Workspace → Tab → Pane structure shown as fixed four-line Workspace blocks. | Workspace list |
| **Current workspace** | The local Workspace that currently owns the focused Herdr surface. | Active project, current project |
| **Previous workspace** | The local Workspace visited immediately before the Current workspace. | Last workspace, previous project |

## Sources

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Source** | A category that supplies Entries to the Result list. | Provider, collection |
| **Workspace source** | The Source of currently open Workspaces and their Tabs. | Open source |
| **Agent source** | The Source of agent panes reported by Herdr. | Agent list |
| **Project source** | The Source of Herdr Plus project definitions. | Template source |
| **Zoxide source** | The Source of directories returned by zoxide. | Directory history |
| **Root source** | The Source of directories discovered below configured filesystem roots. | Scan source |
| **Server source** | The Source of configured remote Herdr targets. | Remote source, SSH source |
| **Session source** | The Source of configured session entries. | Session list |
| **Quick Action source** | The Source of Herdr Plus Quick Actions. | Action menu |
| **Integration source** | The Source of entries collected from an external command or JSON integration. | Plugin source |

## Visual language

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Status glyph** | A symbol that communicates the current state of an agent or Workspace. | Status icon, indicator |
| **Selection marker** | The `→` symbol identifying the Selected entry. | Cursor, pointer |
| **Topology depth** | The selected level in a Topology block: Workspace, Tab, or Pane. | Navigation level |

## Relationships

- A **Picker** has one **Query**, an optional **Source filter**, and a **Result list**.
- A **Popup** and a **Side pane** are two presentations of a **Picker**.
- A **Result list** contains zero or more **Entries**, and each **Entry** belongs to exactly one **Source**.
- An **Entry row** renders exactly one **Entry**; the **Selected entry** is one Entry in the current Result list.
- An **Entry action** is applied to the **Selected entry**.
- A **Topology** contains **Workspaces**; each **Workspace** contains one or more **Tabs**, and each **Tab** contains one or more **Panes**.
- A **Marked entry** is ordered ahead of unmarked Entries within its Source ordering.

## Example dialogue

> **User:** "Why did my workspace not appear in the Picker?"
> **Agent:** "The Workspace source is disabled, or the Source filter is excluding it."
> **User:** "What is the difference between the Entry and the Entry row?"
> **Agent:** "The **Entry** is the destination. The **Entry row** is its visual representation in the Result list."
> **User:** "What happens when I press Enter?"
> **Agent:** "The Picker applies the **Entry action** for the **Selected entry**, such as focusing a Workspace or opening a remote target."
> **User:** "How do I reach a Pane?"
> **Agent:** "The **Topology** shows the Workspace, its Tabs, the selected Tab's Panes, and the selected Pane detail. Enter focuses the exact Pane ID."

## Flagged ambiguities

- "Skill" and "instruction" are used interchangeably in the conversation, but they are different: a **Skill** prescribes a workflow, while an **Instruction** sets a rule.
- "Plugin" can mean any extension in general. Use **Herdr plugin** for the artifact handled by **Herdr**, and **Local plugin** only when it is linked from this repository.
- "Result", "entry", and "row" can describe the same visible thing. Use **Entry** for the destination, **Entry row** for its rendering, and **Result list** for the collection.
- "Query" and "filter" both narrow results. Use **Query** for typed text and **Source filter** for the selected source category.
- "Pin" appears in product-facing language, while the interface command is "mark". Use **Marked entry** and **mark/unmark** for the state and action.
- "Open" is both a Source label and a general verb. Use **Workspace source** for the category and **open** only for the action.
