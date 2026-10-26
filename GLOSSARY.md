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
| **Entry row** | The one-line rendering of an Entry in a flat Result list, as `destination | Source | symbol+word status`. | Result row, list item |
| **Selected entry** | The Entry currently targeted by keyboard or mouse actions. | Highlighted row, active item |
| **Marked entry** | An Entry explicitly kept ahead of normal source ordering. | Pinned item, favorite |
| **Entry action** | The operation performed when the User opens the Selected entry. | Result behavior, command |

## Navigation model

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Herdr session** | One running Herdr server with its own socket, Workspaces and IDs: the default session or a named one. | Server, instance |
| **Other session** | A running Herdr session other than the one Helm runs in; its Workspaces join the Glance only in Needs you. | Remote session |
| **Sessions view** | The Picker view under the `^S` Source filter: one row per Herdr session, running ones before stopped ones, with the selected session's Workspaces in the Detail pane. | Session list, session switcher |
| **Launcher** | `helm-herdr shell`: the process in a terminal tab that runs a Herdr client and, when Helm requests a switch, ends it and attaches another Herdr session in the same tab. | Wrapper, shell session |
| **Workspace** | A Herdr container for related Tabs and their Panes. | Project, session |
| **Tab** | A named view within a Workspace containing one or more Panes. | Workspace tab, window |
| **Pane** | A terminal area within a Tab. | Panel, split |
| **Glance** | The default Picker view: one row per open Workspace, grouped by Warmth group. | Topology view, workspace list |
| **Row label** | A letter from `[glance] labels` that selects its Glance row in one keystroke and opens it on a second press. | Shortcut letter, hotkey |
| **Warmth group** | One of Pinned, Needs you, Hot, Warm, or Cold, which places a Workspace in the Glance. | Bucket, section |
| **Cold workspace** | A Workspace the User sent to Cold by hand; it stays there until its next activity or until warmed up by hand, unless an agent needs you. | Archived, hidden, quiet |
| **Other group** | The group of quick action and plugin Entries, shown in the Glance only while a Query is active. | Other source |
| **Signal** | The age column of a row, showing when the activity that placed it in its Warmth group happened, such as `4m`. | Reason, badge |
| **Topology** | The live Workspace → Tab → Pane structure read from Herdr. | Workspace list |
| **Detail pane** | The part of the Glance beside the list that describes the Selected entry's Workspace: its Tab chips, agents, changes and recent commits. | Side panel, preview |
| **Current workspace** | The local Workspace that currently owns the focused Herdr surface. | Active project, current project |
| **Previous workspace** | The local Workspace visited immediately before the Current workspace. | Last workspace, previous project |

## Sources

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Source** | A category that supplies Entries to the Result list. | Provider, collection |
| **Workspace source** | The Source of currently open Workspaces and their Tabs. | Open source |
| **Session source** | The Source of Herdr sessions, one Entry each, shown only in the Sessions view. | Session list |
| **Zoxide source** | The Source of directories returned by zoxide. | Directory history |
| **Root source** | The Source of directories discovered below configured filesystem roots. | Scan source |
| **Quick Action source** | The Source of Herdr Plus Quick Actions. | Action menu |
| **Integration source** | The Source of entries collected from an external command or JSON integration. | Plugin source |

## Visual language

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Status glyph** | A symbol that communicates the current state of an agent or Workspace. | Status icon, indicator |
| **Selection marker** | The row background that identifies the Selected entry. | Cursor, pointer, arrow |

## Relationships

- A **Picker** has one **Query**, an optional **Source filter**, and a **Result list**.
- A **Popup** and a **Side pane** are two presentations of a **Picker**.
- A **Result list** contains zero or more **Entries**, and each **Entry** belongs to exactly one **Source**.
- An **Entry row** renders exactly one **Entry**; the **Selected entry** is one Entry in the current Result list.
- An **Entry action** is applied to the **Selected entry**.
- A **Topology** contains **Workspaces**; each **Workspace** contains one or more **Tabs**, and each **Tab** contains one or more **Panes**.
- A **Glance** lists each open **Workspace** once, in its **Warmth group**.
- A **Herdr session** contains Workspaces; a Workspace of an **Other session** appears in the **Glance** only while it needs you.
- The **Sessions view** lists every **Herdr session** once; its Detail pane lists that session's Workspaces by **Warmth group**.
- A **Launcher** runs one client of one **Herdr session** at a time; each **Herdr session** has at most one recorded Launcher.
- A **Marked entry** of a **Workspace** sits in the **Pinned** Warmth group, ahead of every other group.

## Example dialogue

> **User:** "Why did my workspace not appear in the Picker?"
> **Agent:** "The Workspace source is disabled, or the Source filter is excluding it."
> **User:** "What is the difference between the Entry and the Entry row?"
> **Agent:** "The **Entry** is the destination. The **Entry row** is its visual representation in the Result list."
> **User:** "What happens when I press Enter?"
> **Agent:** "The Picker applies the **Entry action** for the **Selected entry**, such as focusing a Workspace or running an integration command."
> **User:** "How do I see what needs me?"
> **Agent:** "The **Glance** puts Workspaces that need you in the Needs you group, at the top after Pinned."

## Flagged ambiguities

- "Skill" and "instruction" are used interchangeably in the conversation, but they are different: a **Skill** prescribes a workflow, while an **Instruction** sets a rule.
- "Plugin" can mean any extension in general. Use **Herdr plugin** for the artifact handled by **Herdr**, and **Local plugin** only when it is linked from this repository.
- "Result", "entry", and "row" can describe the same visible thing. Use **Entry** for the destination, **Entry row** for its rendering, and **Result list** for the collection.
- "Query" and "filter" both narrow results. Use **Query** for typed text and **Source filter** for the selected source category.
- "Pin" is the word the Picker shows for the action, while the command is named "mark". Use **Marked entry** for the state and **pin/unpin** for the action.
- "Session" alone means a **Herdr session**, never a Workspace. A Claude session is the agent's own conversation and is named in full.
- "Open" is both a Source label and a general verb. Use **Workspace source** for the category and **open** only for the action.
