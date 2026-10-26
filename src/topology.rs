use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
};

use ratatui::style::Color;
use serde_json::Value;

use crate::model::{Entry, EntryAction, Source};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum AgentState {
    Blocked,
    Working,
    Done,
    Idle,
    Unknown,
}

impl AgentState {
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "blocked" => Self::Blocked,
            "working" => Self::Working,
            "done" => Self::Done,
            "idle" => Self::Idle,
            _ => Self::Unknown,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentMeta {
    pub(crate) name: String,
    pub(crate) state: AgentState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PaneNode {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) cwd: PathBuf,
    pub(crate) focused: bool,
    pub(crate) title: Option<String>,
    pub(crate) agent: Option<AgentMeta>,
    /// The Claude session UUID, which names the transcript directory.
    pub(crate) agent_session: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabNode {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) focused: bool,
    pub(crate) panes: Vec<PaneNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GitHead {
    Branch(String),
    Detached(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitIdentity {
    pub(crate) repo_key: String,
    pub(crate) label: String,
    pub(crate) head: GitHead,
    pub(crate) last_commit_at: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceNode {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) session: Option<String>,
    pub(crate) focused: bool,
    pub(crate) tabs: Vec<TabNode>,
    pub(crate) git: Option<GitIdentity>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OpenTopology {
    pub(crate) workspaces: Vec<WorkspaceNode>,
}

pub(crate) trait GitProbe {
    fn worktree_list(&self, cwd: &Path) -> Result<String, String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeGitProbe;

impl GitProbe for NativeGitProbe {
    fn worktree_list(&self, cwd: &Path) -> Result<String, String> {
        let output = Command::new("git")
            .args([
                "-C",
                &cwd.to_string_lossy(),
                "worktree",
                "list",
                "--porcelain",
            ])
            .output()
            .map_err(|error| error.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }
}

#[derive(Clone, Debug)]
struct Worktree {
    path: PathBuf,
    head: String,
    branch: Option<String>,
    detached: bool,
}

fn value_string(value: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        value
            .get(*name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    })
}

fn result_array<'a>(value: &'a Value, name: &str) -> Option<&'a [Value]> {
    value
        .pointer(&format!("/result/snapshot/{name}"))
        .or_else(|| value.pointer(&format!("/result/{name}")))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
}

fn strip_terminal_title(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            if !character.is_control() {
                output.push(character);
            }
            continue;
        }
        match chars.next() {
            Some(']') => {
                let mut osc = String::new();
                while let Some(character) = chars.next() {
                    if character == '\u{07}' {
                        break;
                    }
                    if character == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                    osc.push(character);
                }
                if let Some(value) = osc.split_once(';').map(|(_, value)| value) {
                    output.push_str(value);
                }
            }
            Some('[') => {
                for character in chars.by_ref() {
                    if character.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            Some(_) | None => {}
        }
    }
    output.trim().to_string()
}

fn agent_from_value(value: &Value) -> Option<AgentMeta> {
    let object = value.as_object();
    let name = value.as_str().map(str::to_string).or_else(|| {
        value_string(value, &["agent", "name"]).or_else(|| {
            object.and_then(|object| {
                object
                    .get("agent")
                    .and_then(|v| v.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
    })?;
    let state = value_string(value, &["agent_status", "state", "status"])
        .map(|value| AgentState::parse(&value))
        .unwrap_or(AgentState::Unknown);
    Some(AgentMeta { name, state })
}

fn pane_agent(pane: &Value, agents_by_pane: &HashMap<&str, &Value>) -> Option<AgentMeta> {
    let embedded = pane.get("agent").filter(|value| !value.is_null());
    let external = pane
        .get("pane_id")
        .and_then(Value::as_str)
        .and_then(|id| agents_by_pane.get(id).copied());
    let external_meta = external.and_then(agent_from_value);
    if let Some(value) = embedded {
        let embedded_meta = agent_from_value(value);
        let embedded_name = value
            .as_str()
            .map(str::to_string)
            .or_else(|| value_string(value, &["agent", "name"]))
            .or_else(|| embedded_meta.as_ref().map(|agent| agent.name.clone()));
        let embedded_state = value_string(value, &["agent_status", "state", "status"])
            .or_else(|| {
                value
                    .get("agent")
                    .and_then(|agent| value_string(agent, &["agent_status", "state", "status"]))
            })
            .or_else(|| value_string(pane, &["agent_status", "state", "status"]))
            .map(|state| AgentState::parse(&state));
        let name =
            embedded_name.or_else(|| external_meta.as_ref().map(|agent| agent.name.clone()))?;
        let state = embedded_state
            .or_else(|| external_meta.as_ref().map(|agent| agent.state))
            .unwrap_or(AgentState::Unknown);
        return Some(AgentMeta { name, state });
    }
    external_meta
}

fn pane_title(pane: &Value) -> Option<String> {
    value_string(pane, &["terminal_title", "title"])
        .map(|title| strip_terminal_title(&title))
        .filter(|title| !title.is_empty())
}

fn snapshot_is_usable(value: &Value) -> bool {
    ["workspaces", "tabs", "panes", "agents"]
        .iter()
        .all(|name| result_array(value, name).is_some())
}

fn parse_workspaces(
    workspaces: &[Value],
    tabs: &[Value],
    panes: &[Value],
    agents: &[Value],
    session: Option<String>,
) -> OpenTopology {
    let agents_by_pane = agents
        .iter()
        .filter_map(|agent| Some((agent.get("pane_id")?.as_str()?, agent)))
        .collect::<HashMap<_, _>>();
    let mut result = Vec::new();
    for workspace in workspaces {
        let Some(id) = value_string(workspace, &["workspace_id", "id"]) else {
            continue;
        };
        let label = value_string(workspace, &["label", "name"]).unwrap_or_else(|| id.clone());
        let focused = workspace
            .get("focused")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let workspace_session =
            value_string(workspace, &["session", "session_name"]).or_else(|| session.clone());
        let workspace_tabs = tabs
            .iter()
            .filter(|tab| tab.get("workspace_id").and_then(Value::as_str) == Some(id.as_str()))
            .map(|tab| {
                let tab_id = value_string(tab, &["tab_id", "id"]).unwrap_or_default();
                let tab_label =
                    value_string(tab, &["label", "name"]).unwrap_or_else(|| tab_id.clone());
                let focused = tab.get("focused").and_then(Value::as_bool).unwrap_or(false);
                let mut tab_panes = panes
                    .iter()
                    .filter(|pane| {
                        pane.get("workspace_id").and_then(Value::as_str) == Some(id.as_str())
                            && pane.get("tab_id").and_then(Value::as_str) == Some(tab_id.as_str())
                    })
                    .map(|pane| {
                        let pane_id = value_string(pane, &["pane_id", "id"]).unwrap_or_default();
                        let external = agents_by_pane.get(pane_id.as_str()).copied();
                        let cwd = value_string(pane, &["foreground_cwd", "cwd"])
                            .or_else(|| {
                                external.and_then(|value| {
                                    value_string(value, &["foreground_cwd", "cwd"])
                                })
                            })
                            .map(PathBuf::from)
                            .unwrap_or_default();
                        let agent = pane_agent(pane, &agents_by_pane);
                        let title = pane_title(pane).or_else(|| {
                            external.and_then(|value| {
                                value_string(value, &["terminal_title", "title"])
                                    .map(|title| strip_terminal_title(&title))
                                    .filter(|title| !title.is_empty())
                            })
                        });
                        PaneNode {
                            id: pane_id,
                            label: value_string(pane, &["label", "name"]).unwrap_or_default(),
                            cwd,
                            focused: pane
                                .get("focused")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            title,
                            agent,
                            agent_session: pane
                                .get("agent_session")
                                .or_else(|| external.and_then(|value| value.get("agent_session")))
                                .and_then(|session| value_string(session, &["value"])),
                        }
                    })
                    .collect::<Vec<_>>();
                tab_panes.sort_by_key(|pane| !pane.focused);
                TabNode {
                    id: tab_id,
                    label: tab_label,
                    focused,
                    panes: std::mem::take(&mut tab_panes),
                }
            })
            .collect::<Vec<_>>();
        result.push(WorkspaceNode {
            id,
            label,
            session: workspace_session,
            focused,
            tabs: workspace_tabs,
            git: None,
        });
    }
    OpenTopology { workspaces: result }
}

pub(crate) fn parse_snapshot(snapshot: &Value, session: Option<String>) -> OpenTopology {
    if !snapshot_is_usable(snapshot) {
        return OpenTopology::default();
    }
    parse_workspaces(
        result_array(snapshot, "workspaces").unwrap_or_default(),
        result_array(snapshot, "tabs").unwrap_or_default(),
        result_array(snapshot, "panes").unwrap_or_default(),
        result_array(snapshot, "agents").unwrap_or_default(),
        session,
    )
}

pub(crate) fn topology_from_values(
    workspace_list: &Value,
    tab_list: &Value,
    pane_list: &Value,
    agent_list: &Value,
    session: Option<String>,
) -> OpenTopology {
    parse_workspaces(
        result_array(workspace_list, "workspaces").unwrap_or_default(),
        result_array(tab_list, "tabs").unwrap_or_default(),
        result_array(pane_list, "panes").unwrap_or_default(),
        result_array(agent_list, "agents").unwrap_or_default(),
        session,
    )
}

pub(crate) fn collect_topology<F>(mut fetch: F, session: Option<String>) -> OpenTopology
where
    F: FnMut(&[&str]) -> Result<Value, String>,
{
    if let Ok(snapshot) = fetch(&["api", "snapshot"]) {
        if snapshot_is_usable(&snapshot) {
            return parse_snapshot(&snapshot, session);
        }
    }
    let workspaces = fetch(&["workspace", "list"]).unwrap_or(Value::Null);
    let tabs = fetch(&["tab", "list"]).unwrap_or(Value::Null);
    let panes = fetch(&["pane", "list"]).unwrap_or(Value::Null);
    let agents = fetch(&["agent", "list"]).unwrap_or(Value::Null);
    topology_from_values(&workspaces, &tabs, &panes, &agents, session)
}

fn parse_worktrees(value: &str) -> Vec<Worktree> {
    value
        .split("\n\n")
        .filter_map(|record| {
            let mut path = None;
            let mut head = None;
            let mut branch = None;
            let mut detached = false;
            for line in record.lines() {
                if let Some(value) = line.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(value));
                } else if let Some(value) = line.strip_prefix("HEAD ") {
                    head = Some(value.to_string());
                } else if let Some(value) = line.strip_prefix("branch ") {
                    branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_string(),
                    );
                } else if line == "detached" {
                    detached = true;
                }
            }
            Some(Worktree {
                path: path?,
                head: head.unwrap_or_default(),
                branch,
                detached,
            })
        })
        .collect()
}

fn canonical_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn path_prefix(path: &Path, root: &Path) -> bool {
    path == root || path.strip_prefix(root).is_ok()
}

fn worktree_identity(worktrees: &[Worktree], cwd: &Path) -> Option<GitIdentity> {
    let cwd = canonical_path(cwd);
    let matched = worktrees
        .iter()
        .filter(|worktree| path_prefix(&cwd, &canonical_path(&worktree.path)))
        .max_by_key(|worktree| canonical_path(&worktree.path).components().count())?;
    let primary = worktrees.first()?;
    let primary_path = canonical_path(&primary.path);
    let repo_key = canonical_path(&primary_path.join(".git"))
        .display()
        .to_string();
    let label = primary_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unnamed")
        .to_string();
    let head = if matched.detached || matched.branch.is_none() {
        GitHead::Detached(matched.head.chars().take(8).collect())
    } else {
        GitHead::Branch(matched.branch.clone().unwrap_or_default())
    };
    Some(GitIdentity {
        repo_key,
        label,
        head,
        last_commit_at: None,
    })
}

fn last_commit_at(cwd: &Path) -> Option<i64> {
    let output = Command::new("git")
        .args(["-C", &cwd.to_string_lossy(), "log", "-1", "--format=%ct"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_commit_time(&String::from_utf8_lossy(&output.stdout))
}

fn parse_commit_time(output: &str) -> Option<i64> {
    output.trim().parse().ok()
}

pub(crate) fn enrich_git<P: GitProbe>(topology: &mut OpenTopology, probe: &P) {
    let mut cache: Vec<(String, Vec<Worktree>)> = Vec::new();
    let mut failed = HashSet::new();
    let mut identity_cwds = Vec::with_capacity(topology.workspaces.len());
    for workspace in &mut topology.workspaces {
        let mut cwd_candidates = workspace
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.iter())
            .collect::<Vec<_>>();
        cwd_candidates.sort_by_key(|pane| !pane.focused);
        let mut identity: Option<(GitIdentity, PathBuf)> = None;
        for pane in cwd_candidates {
            if pane.cwd.as_os_str().is_empty() {
                continue;
            }
            let cwd = canonical_path(&pane.cwd);
            let probe_key = cwd.display().to_string();
            if failed.contains(&probe_key) {
                continue;
            }
            let worktrees = cache
                .iter()
                .find(|(_, worktrees)| {
                    worktrees
                        .iter()
                        .any(|worktree| path_prefix(&cwd, &canonical_path(&worktree.path)))
                })
                .map(|(_, worktrees)| worktrees.clone())
                .or_else(|| match probe.worktree_list(&pane.cwd) {
                    Ok(output) => {
                        let worktrees = parse_worktrees(&output);
                        if worktrees.is_empty() {
                            failed.insert(probe_key.clone());
                            None
                        } else {
                            let repo_key = canonical_path(&worktrees[0].path.join(".git"))
                                .display()
                                .to_string();
                            if !cache.iter().any(|(key, _)| key == &repo_key) {
                                cache.push((repo_key, worktrees.clone()));
                            }
                            Some(worktrees)
                        }
                    }
                    Err(_) => {
                        failed.insert(probe_key.clone());
                        None
                    }
                });
            let Some(worktrees) = worktrees else { continue };
            if let Some(found) = worktree_identity(&worktrees, &cwd) {
                identity = Some((found, pane.cwd.clone()));
                break;
            }
        }
        let (git, cwd) = identity.unzip();
        workspace.git = git;
        identity_cwds.push(cwd);
    }

    std::thread::scope(|scope| {
        for (workspace, cwd) in topology.workspaces.iter_mut().zip(identity_cwds) {
            let (Some(git), Some(cwd)) = (workspace.git.as_mut(), cwd) else {
                continue;
            };
            scope.spawn(move || git.last_commit_at = last_commit_at(&cwd));
        }
    });
}

pub(crate) fn repository_color(
    repository: &str,
    _repositories: &[String],
    theme: &crate::theme::Theme,
) -> Color {
    let candidates = [
        theme.mauve,
        theme.teal,
        theme.blue,
        theme.peach,
        theme.green,
        theme.red,
    ];
    if repository.is_empty() {
        return theme.text;
    }
    let mut unique_colors = Vec::new();
    for color in candidates {
        if !unique_colors.contains(&color) {
            unique_colors.push(color);
        }
    }
    unique_colors.retain(|color| *color != theme.panel_bg && *color != Color::Reset);
    if unique_colors.is_empty() {
        return theme.text;
    }
    let hash = repository
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    unique_colors[(hash as usize) % unique_colors.len()]
}

fn workspace_breadcrumb(workspace: &WorkspaceNode) -> String {
    workspace.git.as_ref().map_or_else(
        || workspace.label.clone(),
        |git| {
            let head = match &git.head {
                GitHead::Branch(branch) => branch,
                GitHead::Detached(sha) => sha,
            };
            format!("{} › {}", git.label, head)
        },
    )
}

pub(crate) fn query_entries(topology: &OpenTopology) -> Vec<Entry> {
    let mut entries = Vec::new();
    for workspace in &topology.workspaces {
        let workspace_path = workspace
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.iter())
            .find_map(|pane| (!pane.cwd.as_os_str().is_empty()).then(|| pane.cwd.clone()))
            .unwrap_or_else(|| PathBuf::from(&workspace.label));
        let repository = workspace_breadcrumb(workspace);
        let repository_key = workspace
            .git
            .as_ref()
            .map(|git| format!("repo-key:{}", git.repo_key));
        entries.push(Entry {
            source: Source::Workspace,
            title: workspace.label.clone(),
            subtitle: repository.clone(),
            path: workspace_path.clone(),
            workspace_id: Some(workspace.id.clone()),
            workspace_label: Some(workspace.label.clone()),
            agent_target: None,
            action: EntryAction::FocusWorkspace {
                session: workspace.session.clone(),
                id: workspace.id.clone(),
            },
            source_label: Some("workspace".into()),
            search_terms: vec![workspace.id.clone(), workspace.label.clone(), repository]
                .into_iter()
                .chain(repository_key.clone())
                .chain(
                    workspace
                        .tabs
                        .iter()
                        .flat_map(|tab| &tab.panes)
                        .filter_map(|pane| pane.agent.as_ref())
                        .flat_map(|agent| {
                            [
                                format!("agent-name:{}", agent.name),
                                format!("agent-status:{}", agent.state.label()),
                            ]
                        }),
                )
                .collect(),
        });
        for tab in &workspace.tabs {
            for pane in &tab.panes {
                let key = format!(
                    "pane:{}:{}",
                    workspace.session.as_deref().unwrap_or("<default>"),
                    pane.id
                );
                let label = if pane.label.is_empty() {
                    "unnamed"
                } else {
                    pane.label.as_str()
                };
                let breadcrumb = format!(
                    "{} › {} › {}",
                    workspace_breadcrumb(workspace),
                    tab.label,
                    label
                );
                let mut search_terms = vec![
                    key,
                    pane.id.clone(),
                    pane.label.clone(),
                    workspace.label.clone(),
                    tab.label.clone(),
                ];
                if let Some(repository_key) = &repository_key {
                    search_terms.push(repository_key.clone());
                }
                if let Some(agent) = &pane.agent {
                    search_terms.push(agent.name.clone());
                    search_terms.push(format!("agent-status:{}", agent.state.label()));
                }
                entries.push(Entry {
                    source: Source::Workspace,
                    title: label.into(),
                    subtitle: breadcrumb,
                    path: pane.cwd.clone(),
                    workspace_id: Some(workspace.id.clone()),
                    workspace_label: Some(workspace.label.clone()),
                    agent_target: pane.agent.as_ref().map(|_| pane.id.clone()),
                    action: EntryAction::FocusPane {
                        session: workspace.session.clone(),
                        id: pane.id.clone(),
                    },
                    source_label: Some("pane".into()),
                    search_terms,
                });
            }
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeGit {
        porcelain: String,
        calls: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl GitProbe for FakeGit {
        fn worktree_list(&self, cwd: &Path) -> Result<String, String> {
            self.calls.borrow_mut().push(cwd.to_path_buf());
            Ok(self.porcelain.clone())
        }
    }

    struct MappingGit {
        responses: HashMap<PathBuf, Result<String, String>>,
        calls: std::cell::RefCell<Vec<PathBuf>>,
    }

    impl GitProbe for MappingGit {
        fn worktree_list(&self, cwd: &Path) -> Result<String, String> {
            self.calls.borrow_mut().push(cwd.to_path_buf());
            self.responses
                .get(cwd)
                .cloned()
                .unwrap_or_else(|| Err("not configured".into()))
        }
    }

    fn snapshot(workspaces: Value, tabs: Value, panes: Value, agents: Value) -> Value {
        serde_json::json!({"result":{"snapshot":{
            "workspaces":workspaces,
            "tabs":tabs,
            "panes":panes,
            "agents":agents
        }}})
    }

    #[test]
    fn snapshot_preserves_tab_order_while_marking_focused_tab() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([
                {"workspace_id":"w1","tab_id":"t1","label":"Code","focused":false},
                {"workspace_id":"w1","tab_id":"t2","label":"Edit","focused":true}
            ]),
            serde_json::json!([]),
            serde_json::json!([]),
        );

        let tabs = &parse_snapshot(&value, None).workspaces[0].tabs;

        assert_eq!(
            tabs.iter()
                .map(|tab| tab.label.as_str())
                .collect::<Vec<_>>(),
            ["Code", "Edit"]
        );
        assert_eq!(tabs.iter().position(|tab| tab.focused), Some(1));
    }

    #[test]
    fn pane_fields_win_over_agent_join_and_agent_only_fields_fill_gaps() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code"}]),
            serde_json::json!([
                {"workspace_id":"w1","tab_id":"t1","pane_id":"p1","label":"Shell","cwd":"/shell","terminal_title":"embedded","agent":{"name":"embedded","state":"done"}},
                {"workspace_id":"w1","tab_id":"t1","pane_id":"p2"}
            ]),
            serde_json::json!([
                {"pane_id":"p1","agent":"external","agent_status":"blocked","cwd":"/external","title":"external"},
                {"pane_id":"p2","agent":"claude","agent_status":"working","cwd":"/agent","title":"\u{1b}]0;Claude\u{07}"}
            ]),
        );
        let topology = parse_snapshot(&value, Some("work".into()));
        let panes = &topology.workspaces[0].tabs[0].panes;
        assert_eq!(panes[0].cwd, PathBuf::from("/shell"));
        assert_eq!(panes[0].title.as_deref(), Some("embedded"));
        assert_eq!(panes[0].agent.as_ref().unwrap().name, "embedded");
        assert_eq!(panes[0].agent.as_ref().unwrap().state, AgentState::Done);
        assert_eq!(panes[1].cwd, PathBuf::from("/agent"));
        assert_eq!(panes[1].title.as_deref(), Some("Claude"));
        assert_eq!(panes[1].agent.as_ref().unwrap().state, AgentState::Working);
    }

    #[test]
    fn embedded_agent_fields_are_completed_by_exact_pane_join() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1","agent":{"state":"working"}}]),
            serde_json::json!([{"pane_id":"p1","agent":"claude","agent_status":"done"}]),
        );
        let pane = &parse_snapshot(&value, None).workspaces[0].tabs[0].panes[0];
        assert_eq!(pane.agent.as_ref().unwrap().name, "claude");
        assert_eq!(pane.agent.as_ref().unwrap().state, AgentState::Working);
    }

    #[test]
    fn embedded_nested_agent_object_retains_agent_metadata() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1","agent":{"agent":{"name":"nested"}}}]),
            serde_json::json!([]),
        );
        let pane = &parse_snapshot(&value, None).workspaces[0].tabs[0].panes[0];
        assert_eq!(pane.agent.as_ref().unwrap().name, "nested");
    }

    #[test]
    fn external_empty_terminal_title_is_ordinary_missing_title() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1"}]),
            serde_json::json!([{"pane_id":"p1","agent":"claude","title":"\u{1b}]0;\u{07}"}]),
        );
        let pane = &parse_snapshot(&value, None).workspaces[0].tabs[0].panes[0];
        assert_eq!(pane.title, None);
    }

    #[test]
    fn malformed_snapshot_falls_back_to_list_commands() {
        let malformed =
            serde_json::json!({"result":{"snapshot":{"workspaces":[],"tabs":[],"panes":[]}}});
        let fallback = |args: &[&str]| -> Result<Value, String> {
            match args {
                ["workspace", "list"] => {
                    Ok(serde_json::json!({"result":{"workspaces":[{"workspace_id":"w1"}]}}))
                }
                ["tab", "list"] => Ok(serde_json::json!({"result":{"tabs":[]}})),
                ["pane", "list"] => Ok(serde_json::json!({"result":{"panes":[]}})),
                ["agent", "list"] => Ok(serde_json::json!({"result":{"agents":[]}})),
                _ => Err("unexpected command".into()),
            }
        };
        let mut calls: Vec<Vec<String>> = Vec::new();
        let topology = collect_topology(
            |args| {
                calls.push(args.iter().map(|arg| (*arg).to_string()).collect());
                if args == ["api", "snapshot"] {
                    Ok(malformed.clone())
                } else {
                    fallback(args)
                }
            },
            None,
        );
        assert_eq!(topology.workspaces[0].id, "w1");
        assert_eq!(
            calls,
            vec![
                vec!["api", "snapshot"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                vec!["workspace", "list"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                vec!["tab", "list"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                vec!["pane", "list"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
                vec!["agent", "list"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>(),
            ]
        );
    }

    #[test]
    fn parses_last_commit_time_and_rejects_non_numeric_output() {
        assert_eq!(parse_commit_time("1700000000\n"), Some(1_700_000_000));
        assert_eq!(parse_commit_time(""), None);
        assert_eq!(parse_commit_time("fatal: not a git repository"), None);
    }

    #[test]
    fn git_identity_matches_nested_cwd_and_detached_head() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Herdr"},{"workspace_id":"w2","label":"Detached"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1"},{"workspace_id":"w2","tab_id":"t2"}]),
            serde_json::json!([
                {"workspace_id":"w1","tab_id":"t1","pane_id":"p1","cwd":"/repo/src/lib","focused":true},
                {"workspace_id":"w2","tab_id":"t2","pane_id":"p2","cwd":"/repo-feature"}
            ]),
            serde_json::json!([]),
        );
        let mut topology = parse_snapshot(&value, None);
        let git = FakeGit { porcelain: "worktree /repo\nHEAD 1234567890abcdef\nbranch refs/heads/main\n\nworktree /repo-feature\nHEAD deadbeefcafebabe\ndetached\n".into(), calls: std::cell::RefCell::new(Vec::new()) };
        enrich_git(&mut topology, &git);
        assert_eq!(topology.workspaces[0].git.as_ref().unwrap().label, "repo");
        assert_eq!(
            topology.workspaces[0].git.as_ref().unwrap().head,
            GitHead::Branch("main".into())
        );
        assert_eq!(
            topology.workspaces[1].git.as_ref().unwrap().head,
            GitHead::Detached("deadbeef".into())
        );
        assert_eq!(git.calls.borrow().len(), 1);
    }

    #[test]
    fn git_probe_tries_remaining_pane_after_focused_non_git_pane() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1"}]),
            serde_json::json!([
                {"workspace_id":"w1","tab_id":"t1","pane_id":"focused","cwd":"/not-git","focused":true},
                {"workspace_id":"w1","tab_id":"t1","pane_id":"other","cwd":"/repo/src"}
            ]),
            serde_json::json!([]),
        );
        let mut topology = parse_snapshot(&value, None);
        let mut responses = HashMap::new();
        responses.insert(PathBuf::from("/not-git"), Err("not a repository".into()));
        responses.insert(
            PathBuf::from("/repo/src"),
            Ok("worktree /repo\nHEAD 12345678\nbranch refs/heads/main\n".into()),
        );
        let git = MappingGit {
            responses,
            calls: std::cell::RefCell::new(Vec::new()),
        };
        enrich_git(&mut topology, &git);
        assert_eq!(
            git.calls.borrow().as_slice(),
            [PathBuf::from("/not-git"), PathBuf::from("/repo/src")]
        );
        assert_eq!(topology.workspaces[0].git.as_ref().unwrap().label, "repo");
    }

    #[test]
    fn non_git_probe_degrades_only_that_workspace() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Plain"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1","cwd":"/plain"}]),
            serde_json::json!([]),
        );
        let mut topology = parse_snapshot(&value, None);
        let mut responses = HashMap::new();
        responses.insert(PathBuf::from("/plain"), Err("not a repository".into()));
        enrich_git(
            &mut topology,
            &MappingGit {
                responses,
                calls: std::cell::RefCell::new(Vec::new()),
            },
        );
        assert_eq!(topology.workspaces[0].git, None);
    }

    #[test]
    fn native_git_probe_reads_temporary_repository() {
        let root = std::env::temp_dir().join(format!("helm-topology-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .status()
                .unwrap();
            assert!(status.success(), "git command failed: {args:?}");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "helm@example.test"]);
        run(&["config", "user.name", "Helm Test"]);
        std::fs::write(root.join("README"), "fixture").unwrap();
        run(&["add", "README"]);
        run(&["commit", "-q", "-m", "fixture"]);
        let porcelain = NativeGitProbe.worktree_list(&root).unwrap();
        assert!(porcelain.starts_with("worktree "));
        assert!(porcelain.contains("HEAD "));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pane_query_entries_use_exact_session_qualified_keys() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1","label":"Shell","cwd":"/tmp"}]),
            serde_json::json!([]),
        );
        let entries = query_entries(&parse_snapshot(&value, Some("session".into())));
        assert!(entries.iter().any(|entry| entry.key() == "pane:session:p1"));
        assert!(
            matches!(entries.last().unwrap().action, EntryAction::FocusPane { ref id, .. } if id == "p1")
        );
    }
}
