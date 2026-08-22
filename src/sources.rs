use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

use crate::{
    config::Config,
    herdr::{herdr_json, herdr_json_args},
    model::{Entry, EntryAction, Source, WorkspaceKind, WorkspaceRef},
    paths::{basename, canonical_str, expand_path, home},
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionSpec {
    name: Option<String>,
}

impl SessionSpec {
    fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("default")
    }

    fn args(&self, command: [&str; 2]) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(name) = &self.name {
            args.extend(["--session".into(), name.clone()]);
        }
        args.extend(command.map(str::to_string));
        args
    }
}

pub(crate) fn collect_open_topology(
    include_topology: bool,
) -> (Vec<Entry>, Vec<Entry>, HashMap<String, Vec<WorkspaceRef>>) {
    let current_name = std::env::var("HERDR_SESSION")
        .ok()
        .filter(|name| !name.trim().is_empty());
    let session = SessionSpec { name: current_name };
    let (ws_json, tab_json, pane_json) =
        session_state_with(&session, include_topology, herdr_json_args);
    let (topology, current_workspaces, current_map) =
        topology_from_json(&session, &ws_json, &tab_json, &pane_json);

    (
        if include_topology {
            topology
        } else {
            Vec::new()
        },
        current_workspaces,
        current_map,
    )
}

fn snapshot_is_usable(value: &Value, include_tabs: bool) -> bool {
    value
        .pointer("/result/snapshot/workspaces")
        .is_some_and(Value::is_array)
        && value
            .pointer("/result/snapshot/panes")
            .is_some_and(Value::is_array)
        && (!include_tabs
            || value
                .pointer("/result/snapshot/tabs")
                .is_some_and(Value::is_array))
}

fn session_state_with<F>(
    session: &SessionSpec,
    include_tabs: bool,
    mut fetch: F,
) -> (Value, Value, Value)
where
    F: FnMut(Vec<String>) -> Result<Value, String>,
{
    if let Ok(snapshot) = fetch(session.args(["api", "snapshot"])) {
        if snapshot_is_usable(&snapshot, include_tabs) {
            let tabs = if include_tabs {
                snapshot.clone()
            } else {
                Value::Null
            };
            return (snapshot.clone(), tabs, snapshot);
        }
    }

    let workspaces = fetch(session.args(["workspace", "list"])).unwrap_or(Value::Null);
    let tabs = if include_tabs {
        fetch(session.args(["tab", "list"])).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let panes = fetch(session.args(["pane", "list"])).unwrap_or(Value::Null);
    (workspaces, tabs, panes)
}

fn result_array<'a>(value: &'a Value, name: &str) -> Option<&'a Vec<Value>> {
    value
        .pointer(&format!("/result/snapshot/{name}"))
        .or_else(|| value.pointer(&format!("/result/{name}")))
        .and_then(Value::as_array)
}

#[derive(Clone, Debug)]
struct WorkspaceTopologyBlock {
    entry: Entry,
    tabs: Vec<Entry>,
    parent_workspace_id: Option<String>,
}

fn worktree_repo_key(workspace: &Value) -> Option<&str> {
    workspace
        .pointer("/worktree/repo_key")
        .and_then(Value::as_str)
        .filter(|repo_key| !repo_key.is_empty())
}

fn is_linked_worktree(workspace: &Value) -> Option<bool> {
    workspace
        .pointer("/worktree/is_linked_worktree")
        .and_then(Value::as_bool)
}

fn topology_from_json(
    session: &SessionSpec,
    ws_json: &Value,
    tab_json: &Value,
    pane_json: &Value,
) -> (Vec<Entry>, Vec<Entry>, HashMap<String, Vec<WorkspaceRef>>) {
    let mut entries = Vec::new();
    let mut workspace_entries = Vec::new();
    let mut map = HashMap::new();
    let mut cwd_by_ws: HashMap<String, String> = HashMap::new();
    if let Some(panes) = result_array(pane_json, "panes") {
        for pane in panes {
            let Some(workspace_id) = pane.get("workspace_id").and_then(Value::as_str) else {
                continue;
            };
            let cwd = pane
                .get("foreground_cwd")
                .or_else(|| pane.get("cwd"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !cwd.is_empty() {
                cwd_by_ws.entry(workspace_id.into()).or_insert(cwd.into());
            }
        }
    }

    let workspaces = result_array(ws_json, "workspaces")
        .cloned()
        .unwrap_or_default();
    let tabs = result_array(tab_json, "tabs").cloned().unwrap_or_default();
    let mut canonical_parent_by_repo = HashMap::new();
    for workspace in &workspaces {
        let id = workspace
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if id.is_empty() || is_linked_worktree(workspace) != Some(false) {
            continue;
        }
        if let Some(repo_key) = worktree_repo_key(workspace) {
            canonical_parent_by_repo
                .entry(repo_key.to_string())
                .or_insert_with(|| id.to_string());
        }
    }

    let mut blocks = Vec::new();
    for workspace in &workspaces {
        let id = workspace
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let label = workspace.get("label").and_then(Value::as_str).unwrap_or(id);
        let cwd = cwd_by_ws
            .get(id)
            .cloned()
            .unwrap_or_else(|| home().display().to_string());
        let path = PathBuf::from(&cwd);
        let tab_count = workspace
            .get("tab_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let pane_count = workspace
            .get("pane_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let agent_status = workspace
            .get("agent_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let focused = workspace
            .get("focused")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let repo_key = worktree_repo_key(workspace);
        let linked_worktree = is_linked_worktree(workspace) == Some(true);
        let parent_workspace_id = if linked_worktree {
            repo_key
                .and_then(|repo_key| canonical_parent_by_repo.get(repo_key))
                .filter(|parent_id| parent_id.as_str() != id)
                .cloned()
        } else {
            None
        };
        let mut search_terms = vec![
            id.into(),
            label.into(),
            agent_status.into(),
            session.label().into(),
            "workspace".into(),
        ];
        if let Some(repo_key) = repo_key {
            search_terms.push(repo_key.into());
        }
        if linked_worktree {
            search_terms.push("worktree".into());
        }
        if focused {
            search_terms.push("focused".into());
        }
        let entry = Entry {
            source: Source::Workspace,
            title: label.into(),
            subtitle: format!("agent:{agent_status} · {id} tabs:{tab_count} panes:{pane_count}"),
            path: path.clone(),
            workspace_id: Some(id.into()),
            workspace_label: Some(label.into()),
            agent_target: None,
            project: None,
            action: EntryAction::FocusWorkspace {
                session: session.name.clone(),
                id: id.into(),
            },
            source_label: None,
            search_terms,
        };
        if let Some(key) = canonical_str(&path) {
            map.entry(key).or_insert_with(Vec::new).push(WorkspaceRef {
                id: id.into(),
                label: label.into(),
                kind: workspace_kind(label),
                path: path.clone(),
                tab_count,
                pane_count,
            });
        }
        workspace_entries.push(entry.clone());

        let mut workspace_tabs = tabs
            .iter()
            .filter(|tab| tab.get("workspace_id").and_then(Value::as_str) == Some(id))
            .collect::<Vec<_>>();
        workspace_tabs.sort_by_key(|tab| {
            tab.get("number")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX)
        });
        let tab_entries = workspace_tabs
            .into_iter()
            .map(|tab| {
                let tab_id = tab.get("tab_id").and_then(Value::as_str).unwrap_or("");
                let tab_label = tab.get("label").and_then(Value::as_str).unwrap_or(tab_id);
                let tab_panes = tab.get("pane_count").and_then(Value::as_i64).unwrap_or(0);
                Entry {
                    source: Source::Workspace,
                    title: tab_label.into(),
                    subtitle: format!("{tab_id} · {tab_panes} panes"),
                    path: path.clone(),
                    workspace_id: Some(id.into()),
                    workspace_label: Some(label.into()),
                    agent_target: None,
                    project: None,
                    action: EntryAction::FocusTab {
                        session: session.name.clone(),
                        id: tab_id.into(),
                    },
                    source_label: None,
                    search_terms: vec![
                        tab_id.into(),
                        tab_label.into(),
                        label.into(),
                        session.label().into(),
                        "tab".into(),
                    ],
                }
            })
            .collect();
        blocks.push(WorkspaceTopologyBlock {
            entry,
            tabs: tab_entries,
            parent_workspace_id,
        });
    }

    for parent in blocks
        .iter()
        .filter(|block| block.parent_workspace_id.is_none())
    {
        entries.push(parent.entry.clone());
        entries.extend(parent.tabs.iter().cloned());
        let parent_id = parent.entry.workspace_id.as_deref().unwrap_or("");
        for child in blocks
            .iter()
            .filter(|block| block.parent_workspace_id.as_deref() == Some(parent_id))
        {
            entries.push(child.entry.clone());
            entries.extend(child.tabs.iter().cloned());
        }
    }
    (entries, workspace_entries, map)
}

fn workspace_kind(label: &str) -> WorkspaceKind {
    let label = label.trim().to_ascii_lowercase();
    if label.starts_with("project:") {
        WorkspaceKind::Project
    } else if label.starts_with("dir:") {
        WorkspaceKind::Dir
    } else {
        WorkspaceKind::Unknown
    }
}

pub(crate) fn collect_agents(
    workspaces: &[Entry],
    aliases: &[crate::config::AgentAliasConfig],
) -> Vec<Entry> {
    let agent_json = herdr_json(["agent", "list"]).unwrap_or(Value::Null);
    agents_from_json(&agent_json, workspaces, aliases)
}

fn agents_from_json(
    agent_json: &Value,
    workspaces: &[Entry],
    aliases: &[crate::config::AgentAliasConfig],
) -> Vec<Entry> {
    let workspace_labels: HashMap<&str, &str> = workspaces
        .iter()
        .filter_map(|entry| Some((entry.workspace_id.as_deref()?, entry.title.as_str())))
        .collect();
    let mut entries = Vec::new();
    if let Some(agents) = agent_json
        .pointer("/result/agents")
        .and_then(|v| v.as_array())
    {
        for p in agents {
            let Some(agent) = p.get("agent").and_then(|v| v.as_str()) else {
                continue;
            };
            let pane = p.get("pane_id").and_then(|v| v.as_str()).unwrap_or("");
            let tab = p.get("tab_id").and_then(|v| v.as_str()).unwrap_or("");
            let term = p.get("terminal_id").and_then(|v| v.as_str()).unwrap_or("");
            // Herdr's `agent focus` accepts pane IDs, not terminal IDs.
            let target = pane;
            let cwd = p.get("cwd").and_then(|v| v.as_str()).unwrap_or("/");
            let foreground_cwd = p
                .get("foreground_cwd")
                .and_then(|v| v.as_str())
                .unwrap_or(cwd);
            let status = p
                .get("agent_status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let workspace_id = p.get("workspace_id").and_then(|v| v.as_str()).unwrap_or("");
            let workspace_label = workspace_labels
                .get(workspace_id)
                .copied()
                .unwrap_or(workspace_id);
            let path = PathBuf::from(cwd);
            let dir = basename(&path);
            let alias_terms: Vec<String> = aliases
                .iter()
                .filter(|alias| alias.matches(agent, workspace_label, cwd))
                .map(|alias| alias.alias.clone())
                .collect();
            let title = format!("{agent} · {workspace_label} · {dir}");
            let subtitle = format!("{status} · {pane} · {tab}");
            let mut search_terms = vec![
                agent.into(),
                status.into(),
                pane.into(),
                tab.into(),
                term.into(),
                workspace_id.into(),
                workspace_label.into(),
                dir,
                basename(&PathBuf::from(foreground_cwd)),
                foreground_cwd.into(),
            ];
            if let Some(session) = p.pointer("/agent_session/value").and_then(|v| v.as_str()) {
                search_terms.push(session.into());
            }
            search_terms.extend(alias_terms);
            entries.push(Entry {
                source: Source::Agent,
                title,
                subtitle,
                path,
                workspace_id: (!workspace_id.is_empty()).then(|| workspace_id.into()),
                workspace_label: Some(workspace_label.into()),
                agent_target: Some(target.into()),
                project: None,
                action: EntryAction::FocusAgent {
                    target: target.into(),
                },
                source_label: Some("pane".into()),
                search_terms,
            });
        }
    }
    entries
}

const AGENT_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// Mirrors Herdr's workspace `state_dot` and agent `agent_icon` mappings.
pub(crate) fn status_icon_at(source: &Source, status: &str, tick: u32) -> &'static str {
    let workspace = *source == Source::Workspace;
    let status = status.to_lowercase();
    if status.contains("block")
        || status.contains("error")
        || status.contains("fail")
        || status.contains("attention")
        || status.contains("request")
        || status.contains("wait")
    {
        if workspace {
            "●"
        } else {
            "!"
        }
    } else if status.contains("work") || status.contains("run") {
        if workspace {
            "●"
        } else {
            AGENT_SPINNER[tick as usize % AGENT_SPINNER.len()]
        }
    } else if status.contains("done") || status.contains("complete") {
        if workspace {
            "●"
        } else {
            "✓"
        }
    } else if status.contains("idle") {
        "○"
    } else if workspace {
        "·"
    } else {
        ""
    }
}

pub(crate) fn collect_zoxide() -> Vec<Entry> {
    let Ok(out) = Command::new("zoxide").args(["query", "-l"]).output() else {
        return vec![];
    };
    if !out.status.success() {
        return vec![];
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let path = PathBuf::from(line);
            Entry {
                source: Source::Zoxide,
                title: basename(&path),
                subtitle: line.into(),
                path,
                workspace_id: None,
                workspace_label: None,
                agent_target: None,
                project: None,
                action: EntryAction::FocusOrCreateDir,
                source_label: None,
                search_terms: vec![],
            }
        })
        .collect()
}

pub(crate) fn collect_roots(config: &Config) -> Vec<Entry> {
    let mut out = Vec::new();
    for root in &config.roots {
        walk_dirs(&expand_path(&root.path), root.max_depth, &mut out);
    }
    out
}
fn walk_dirs(path: &Path, depth: usize, out: &mut Vec<Entry>) {
    if depth == 0 || !path.is_dir() {
        return;
    }
    if path.join(".git").exists()
        || path.join("package.json").exists()
        || path.join("Cargo.toml").exists()
    {
        out.push(Entry {
            source: Source::Root,
            title: basename(path),
            subtitle: path.display().to_string(),
            path: path.to_path_buf(),
            workspace_id: None,
            workspace_label: None,
            agent_target: None,
            project: None,
            action: EntryAction::FocusOrCreateDir,
            source_label: None,
            search_terms: vec![],
        });
    }
    if let Ok(read) = fs::read_dir(path) {
        for e in read.flatten() {
            let p = e.path();
            if p.is_dir() && !basename(&p).starts_with('.') {
                walk_dirs(&p, depth - 1, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ponytail: fixtures captured from Herdr's workspace/tab/pane/agent list commands.

    #[test]
    fn snapshot_supplies_workspace_tab_and_pane_topology_in_one_read() {
        let session = SessionSpec {
            name: Some("work".into()),
        };
        let snapshot = serde_json::json!({"result":{"snapshot":{
            "workspaces":[{"workspace_id":"w1","label":"Project","focused":true,"tab_count":1,"pane_count":1}],
            "tabs":[{"workspace_id":"w1","tab_id":"w1:t1","label":"Code","number":1,"focused":true,"pane_count":1}],
            "panes":[{"workspace_id":"w1","foreground_cwd":"/tmp/project"}]
        }}});
        let mut calls = Vec::new();
        let (workspaces, tabs, panes) = session_state_with(&session, true, |args| {
            calls.push(args);
            Ok(snapshot.clone())
        });

        assert_eq!(calls, vec![vec!["--session", "work", "api", "snapshot"]]);
        let (topology, current, _) = topology_from_json(&session, &workspaces, &tabs, &panes);
        assert_eq!(topology.len(), 2);
        assert_eq!(current[0].path, PathBuf::from("/tmp/project"));
        assert_eq!(topology[1].title, "Code");
    }

    #[test]
    fn snapshot_failure_falls_back_to_list_commands() {
        let session = SessionSpec {
            name: Some("work".into()),
        };
        let mut calls = Vec::new();
        let _ = session_state_with(&session, true, |args| {
            calls.push(args.clone());
            if args.ends_with(&["api".into(), "snapshot".into()]) {
                Err("unsupported".into())
            } else {
                Ok(serde_json::json!({"result":{}}))
            }
        });

        assert_eq!(calls.len(), 4);
        assert!(calls[1].ends_with(&["workspace".into(), "list".into()]));
        assert!(calls[2].ends_with(&["tab".into(), "list".into()]));
        assert!(calls[3].ends_with(&["pane".into(), "list".into()]));
    }

    #[test]
    fn malformed_snapshot_arrays_fall_back_to_list_commands() {
        let session = SessionSpec {
            name: Some("work".into()),
        };
        let malformed = [
            serde_json::json!({"result":{"snapshot":{
                "workspaces":null, "tabs":[], "panes":[]
            }}}),
            serde_json::json!({"result":{"snapshot":{
                "workspaces":[], "tabs":[], "panes":{}
            }}}),
            serde_json::json!({"result":{"snapshot":{
                "workspaces":[], "tabs":null, "panes":[]
            }}}),
        ];

        for snapshot in malformed {
            let mut calls = Vec::new();
            let _ = session_state_with(&session, true, |args| {
                calls.push(args.clone());
                if args.ends_with(&["api".into(), "snapshot".into()]) {
                    Ok(snapshot.clone())
                } else {
                    Ok(serde_json::json!({"result":{}}))
                }
            });

            assert_eq!(calls.len(), 4);
            assert!(calls[1].ends_with(&["workspace".into(), "list".into()]));
            assert!(calls[2].ends_with(&["tab".into(), "list".into()]));
            assert!(calls[3].ends_with(&["pane".into(), "list".into()]));
        }
    }

    #[test]
    fn disabled_open_collection_skips_tabs() {
        let session = SessionSpec {
            name: Some("current".into()),
        };

        let snapshot = serde_json::json!({"result":{"snapshot":{
            "workspaces":[], "tabs":[{"tab_id":"ignored"}], "panes":[]
        }}});
        let mut calls = Vec::new();
        let (_, tabs, _) = session_state_with(&session, false, |args| {
            calls.push(args);
            Ok(snapshot.clone())
        });
        assert_eq!(calls.len(), 1);
        assert!(tabs.is_null());
    }

    #[test]
    fn status_icons_match_herdr() {
        assert_eq!(status_icon_at(&Source::Workspace, "blocked", 0), "●");
        assert_eq!(status_icon_at(&Source::Workspace, "working", 0), "●");
        assert_eq!(status_icon_at(&Source::Workspace, "done", 0), "●");
        assert_eq!(status_icon_at(&Source::Workspace, "idle", 0), "○");
        assert_eq!(status_icon_at(&Source::Workspace, "unknown", 0), "·");

        assert_eq!(status_icon_at(&Source::Agent, "blocked", 0), "!");
        assert_eq!(status_icon_at(&Source::Agent, "working", 0), "⠋");
        assert_eq!(status_icon_at(&Source::Agent, "working", 1), "⠙");
        assert_eq!(status_icon_at(&Source::Agent, "done", 0), "✓");
        assert_eq!(status_icon_at(&Source::Agent, "idle", 0), "○");
        assert_eq!(status_icon_at(&Source::Agent, "unknown", 0), "");
    }
}
