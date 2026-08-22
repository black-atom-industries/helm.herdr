#![allow(dead_code)]

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
};

use ratatui::text::Span;
use serde_json::Value;

use crate::{
    config::AgentAliasConfig,
    model::{Entry, EntryAction, Source},
};

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

    fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Working => "working",
            Self::Idle => "idle",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn glyph(self) -> &'static str {
        match self {
            Self::Blocked => "!",
            Self::Working => "⠋",
            Self::Done => "✓",
            Self::Idle => "○",
            Self::Unknown => "",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AgentMeta {
    pub(crate) name: String,
    pub(crate) state: AgentState,
    pub(crate) alias: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PaneNode {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) cwd: PathBuf,
    pub(crate) focused: bool,
    pub(crate) title: Option<String>,
    pub(crate) agent: Option<AgentMeta>,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TopologyDepth {
    #[default]
    Workspace,
    Tab,
    Pane,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChildSelection {
    pub(crate) tab: usize,
    pub(crate) pane: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TopologyCursor {
    pub(crate) workspace: usize,
    pub(crate) depth: TopologyDepth,
    pub(crate) selection: Vec<ChildSelection>,
}

impl Default for TopologyCursor {
    fn default() -> Self {
        Self {
            workspace: 0,
            depth: TopologyDepth::Workspace,
            selection: Vec::new(),
        }
    }
}

impl TopologyCursor {
    pub(crate) fn new(topology: &OpenTopology) -> Self {
        let mut cursor = Self {
            selection: vec![ChildSelection::default(); topology.workspaces.len()],
            ..Self::default()
        };
        cursor.clamp(topology);
        cursor
    }

    pub(crate) fn enter_tab(&mut self, topology: &OpenTopology) {
        self.clamp(topology);
        if !topology.workspaces.is_empty() && !topology.workspaces[self.workspace].tabs.is_empty() {
            self.depth = TopologyDepth::Tab;
        }
    }

    pub(crate) fn enter_pane(&mut self, topology: &OpenTopology) {
        self.clamp(topology);
        let Some(tab) = topology.workspaces[self.workspace]
            .tabs
            .get(self.selection[self.workspace].tab)
        else {
            return;
        };
        if !tab.panes.is_empty() {
            self.depth = TopologyDepth::Pane;
        }
    }

    pub(crate) fn leave_to_workspace(&mut self) {
        self.depth = TopologyDepth::Workspace;
    }

    pub(crate) fn leave_to_tab(&mut self) {
        self.depth = TopologyDepth::Tab;
    }

    pub(crate) fn move_workspace(&mut self, topology: &OpenTopology, delta: isize) {
        if topology.workspaces.is_empty() {
            return;
        }
        let last = topology.workspaces.len() - 1;
        self.workspace = if delta.is_negative() {
            self.workspace.saturating_sub(delta.unsigned_abs())
        } else {
            self.workspace.saturating_add(delta as usize).min(last)
        };
        self.clamp(topology);
    }

    pub(crate) fn move_tab(&mut self, topology: &OpenTopology, delta: isize) {
        self.clamp(topology);
        let tabs = &topology.workspaces[self.workspace].tabs;
        if tabs.is_empty() {
            return;
        }
        let selected = &mut self.selection[self.workspace].tab;
        *selected = if delta.is_negative() {
            selected.saturating_sub(delta.unsigned_abs())
        } else {
            selected.saturating_add(delta as usize).min(tabs.len() - 1)
        };
        self.selection[self.workspace].pane = 0;
        self.clamp(topology);
    }

    pub(crate) fn move_pane(&mut self, topology: &OpenTopology, delta: isize) {
        self.clamp(topology);
        let selected = &mut self.selection[self.workspace];
        let Some(panes) = topology.workspaces[self.workspace]
            .tabs
            .get(selected.tab)
            .map(|tab| &tab.panes)
        else {
            return;
        };
        if panes.is_empty() {
            return;
        }
        selected.pane = if delta.is_negative() {
            selected.pane.saturating_sub(delta.unsigned_abs())
        } else {
            selected
                .pane
                .saturating_add(delta as usize)
                .min(panes.len() - 1)
        };
    }

    pub(crate) fn clamp(&mut self, topology: &OpenTopology) {
        if topology.workspaces.is_empty() {
            self.workspace = 0;
            self.depth = TopologyDepth::Workspace;
            self.selection.clear();
            return;
        }
        self.workspace = self.workspace.min(topology.workspaces.len() - 1);
        self.selection
            .resize(topology.workspaces.len(), ChildSelection::default());
        let workspace = &topology.workspaces[self.workspace];
        let selected = &mut self.selection[self.workspace];
        selected.tab = selected.tab.min(workspace.tabs.len().saturating_sub(1));
        selected.pane = workspace
            .tabs
            .get(selected.tab)
            .map(|tab| selected.pane.min(tab.panes.len().saturating_sub(1)))
            .unwrap_or(0);
        if matches!(self.depth, TopologyDepth::Tab) && workspace.tabs.is_empty() {
            self.depth = TopologyDepth::Workspace;
        }
        if matches!(self.depth, TopologyDepth::Pane)
            && workspace
                .tabs
                .get(selected.tab)
                .is_none_or(|tab| tab.panes.is_empty())
        {
            self.depth = TopologyDepth::Tab;
        }
    }
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

fn alias_for(
    agent: &str,
    workspace: &str,
    cwd: &Path,
    aliases: &[AgentAliasConfig],
) -> Option<String> {
    aliases
        .iter()
        .find(|alias| alias.matches(agent, workspace, &cwd.to_string_lossy()))
        .map(|alias| alias.alias.clone())
}

fn agent_from_value(
    value: &Value,
    workspace: &str,
    cwd: &Path,
    aliases: &[AgentAliasConfig],
) -> Option<AgentMeta> {
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
    Some(AgentMeta {
        alias: alias_for(&name, workspace, cwd, aliases),
        name,
        state,
    })
}

fn pane_agent(
    pane: &Value,
    workspace: &str,
    cwd: &Path,
    agents_by_pane: &HashMap<&str, &Value>,
    aliases: &[AgentAliasConfig],
) -> Option<AgentMeta> {
    let embedded = pane.get("agent").filter(|value| !value.is_null());
    let external = pane
        .get("pane_id")
        .and_then(Value::as_str)
        .and_then(|id| agents_by_pane.get(id).copied());
    let external_meta = external.and_then(|value| agent_from_value(value, workspace, cwd, aliases));
    if let Some(value) = embedded {
        let embedded_meta = agent_from_value(value, workspace, cwd, aliases);
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
        return Some(AgentMeta {
            alias: alias_for(&name, workspace, cwd, aliases),
            name,
            state,
        });
    }
    external_meta
}

fn pane_title(pane: &Value) -> Option<String> {
    value_string(pane, &["terminal_title", "title"])
        .map(|title| strip_terminal_title(&title))
        .filter(|title| !title.is_empty())
}

fn snapshot_is_usable(value: &Value, include_agents: bool) -> bool {
    ["workspaces", "tabs", "panes"]
        .iter()
        .all(|name| result_array(value, name).is_some())
        && (!include_agents || result_array(value, "agents").is_some())
}

fn parse_workspaces(
    workspaces: &[Value],
    tabs: &[Value],
    panes: &[Value],
    agents: &[Value],
    session: Option<String>,
    aliases: &[AgentAliasConfig],
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
        let mut workspace_tabs = tabs
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
                        let agent = pane_agent(pane, &label, &cwd, &agents_by_pane, aliases);
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
        workspace_tabs.sort_by_key(|tab| !tab.focused);
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

fn parse_snapshot_with(
    snapshot: &Value,
    session: Option<String>,
    aliases: &[AgentAliasConfig],
    include_agents: bool,
) -> OpenTopology {
    if !snapshot_is_usable(snapshot, include_agents) {
        return OpenTopology::default();
    }
    parse_workspaces(
        result_array(snapshot, "workspaces").unwrap_or_default(),
        result_array(snapshot, "tabs").unwrap_or_default(),
        result_array(snapshot, "panes").unwrap_or_default(),
        result_array(snapshot, "agents").unwrap_or_default(),
        session,
        aliases,
    )
}

pub(crate) fn parse_snapshot(
    snapshot: &Value,
    session: Option<String>,
    aliases: &[AgentAliasConfig],
) -> OpenTopology {
    parse_snapshot_with(snapshot, session, aliases, true)
}

pub(crate) fn topology_from_values(
    workspace_list: &Value,
    tab_list: &Value,
    pane_list: &Value,
    agent_list: &Value,
    session: Option<String>,
    aliases: &[AgentAliasConfig],
) -> OpenTopology {
    parse_workspaces(
        result_array(workspace_list, "workspaces").unwrap_or_default(),
        result_array(tab_list, "tabs").unwrap_or_default(),
        result_array(pane_list, "panes").unwrap_or_default(),
        result_array(agent_list, "agents").unwrap_or_default(),
        session,
        aliases,
    )
}

pub(crate) fn collect_topology<F>(
    mut fetch: F,
    session: Option<String>,
    aliases: &[AgentAliasConfig],
    include_agents: bool,
) -> OpenTopology
where
    F: FnMut(&[&str]) -> Result<Value, String>,
{
    if let Ok(snapshot) = fetch(&["api", "snapshot"]) {
        if snapshot_is_usable(&snapshot, include_agents) {
            let mut topology = parse_snapshot_with(&snapshot, session, aliases, include_agents);
            if !include_agents {
                for workspace in &mut topology.workspaces {
                    for tab in &mut workspace.tabs {
                        for pane in &mut tab.panes {
                            pane.agent = None;
                        }
                    }
                }
            }
            return topology;
        }
    }
    let workspaces = fetch(&["workspace", "list"]).unwrap_or(Value::Null);
    let tabs = fetch(&["tab", "list"]).unwrap_or(Value::Null);
    let panes = fetch(&["pane", "list"]).unwrap_or(Value::Null);
    let agents = if include_agents {
        fetch(&["agent", "list"]).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    topology_from_values(&workspaces, &tabs, &panes, &agents, session, aliases)
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
    })
}

pub(crate) fn enrich_git<P: GitProbe>(topology: &mut OpenTopology, probe: &P) {
    let mut cache: Vec<(String, Vec<Worktree>)> = Vec::new();
    let mut failed = HashSet::new();
    for workspace in &mut topology.workspaces {
        let mut cwd_candidates = workspace
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.iter())
            .collect::<Vec<_>>();
        cwd_candidates.sort_by_key(|pane| !pane.focused);
        let mut identity = None;
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
                identity = Some(found);
                break;
            }
        }
        workspace.git = identity;
    }
}

pub(crate) fn clip_selected_for_hits(value: &str, width: usize, selected: Option<&str>) -> String {
    if Span::raw(value).width() <= width {
        return value.into();
    }
    if let Some(selected) = selected {
        if let Some(start) = value.find(selected) {
            let suffix = &value[start..];
            return clip(suffix, width);
        }
    }
    clip(value, width)
}

fn clip(value: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used: usize = 0;
    for character in value.chars() {
        let char_width = Span::raw(character.to_string()).width();
        if used.saturating_add(char_width) > width {
            break;
        }
        output.push(character);
        used += char_width;
    }
    output
}

fn aggregate(workspace: &WorkspaceNode) -> String {
    let mut counts = [0usize; 5];
    for pane in workspace.tabs.iter().flat_map(|tab| tab.panes.iter()) {
        if let Some(agent) = &pane.agent {
            counts[agent.state as usize] += 1;
        }
    }
    let states = [
        AgentState::Blocked,
        AgentState::Done,
        AgentState::Working,
        AgentState::Idle,
    ];
    let mut values = states
        .iter()
        .filter_map(|state| {
            let count = counts[*state as usize];
            (count > 0).then(|| format!("{count} {}", state.label()))
        })
        .collect::<Vec<_>>();
    let detected = counts.iter().sum::<usize>();
    if detected > 0 && values.is_empty() {
        values.push(format!("{} unknown", counts[AgentState::Unknown as usize]));
    }
    values.join(" · ")
}

fn pane_value(pane: &PaneNode) -> String {
    let label = if pane.label.is_empty() {
        "unnamed"
    } else {
        &pane.label
    };
    match &pane.agent {
        Some(agent) => format!(
            "{} {}",
            agent.state.glyph(),
            agent.alias.as_deref().unwrap_or(&agent.name)
        ),
        None => label.to_string(),
    }
}

pub(crate) fn render_workspace_block(workspace: &WorkspaceNode, width: usize) -> String {
    render_workspace_block_selected(workspace, width, None)
}

pub(crate) fn render_workspace_block_selected(
    workspace: &WorkspaceNode,
    width: usize,
    selection: Option<ChildSelection>,
) -> String {
    let identity = workspace.git.as_ref().map_or_else(
        || workspace.label.clone(),
        |git| {
            format!(
                "{} › {}",
                git.label,
                match &git.head {
                    GitHead::Branch(branch) => branch,
                    GitHead::Detached(head) => head,
                }
            )
        },
    );
    let state = if workspace.focused { "@" } else { "" };
    let state_width = 5;
    let identity_width = width.saturating_sub(state_width);
    let identity = clip(&identity, identity_width);
    let mut first = format!("{state:^5}{identity}");
    let aggregate = aggregate(workspace);
    let available = width.saturating_sub(Span::raw(&first).width());
    first.push_str(&clip(&aggregate, available));
    let selected_tab_index = selection
        .map(|selection| selection.tab)
        .filter(|index| *index < workspace.tabs.len())
        .or_else(|| workspace.tabs.iter().position(|tab| tab.focused))
        .or_else(|| (!workspace.tabs.is_empty()).then_some(0));
    let selected_tab = selected_tab_index.and_then(|index| workspace.tabs.get(index));
    let tabs = if workspace.tabs.is_empty() {
        "no tabs".into()
    } else {
        workspace
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let label = if tab.label.is_empty() {
                    "unnamed"
                } else {
                    tab.label.as_str()
                };
                if selection.is_some_and(|selection| selection.tab == index) {
                    format!("[{label}]")
                } else {
                    label.into()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let panes = selected_tab
        .map(|tab| {
            if tab.panes.is_empty() {
                "no panes".into()
            } else {
                tab.panes
                    .iter()
                    .enumerate()
                    .map(|(index, pane)| {
                        let value = pane_value(pane);
                        if selection.is_some_and(|selection| selection.pane == index) {
                            format!("[{value}]")
                        } else {
                            value
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
        })
        .unwrap_or_else(|| "no panes".into());
    let detail = selected_tab
        .and_then(|tab| {
            let selected_pane = selection
                .map(|selection| selection.pane)
                .and_then(|index| tab.panes.get(index));
            selected_pane
                .or_else(|| tab.panes.iter().find(|pane| pane.focused))
                .or_else(|| tab.panes.first())
        })
        .map(|pane| match &pane.agent {
            Some(agent) => format!(
                "agent {}",
                agent
                    .alias
                    .as_deref()
                    .or(pane.title.as_deref())
                    .unwrap_or(&agent.name)
            ),
            None => format!(
                "pane {}",
                if pane.label.is_empty() {
                    "unnamed"
                } else {
                    pane.label.as_str()
                }
            ),
        })
        .unwrap_or_else(|| "pane no panes".into());
    let child_prefix = " ".repeat(1);
    let child = |label: &str, value: &str, selected: Option<&str>| {
        let fixed = format!("{child_prefix}{label:<8}");
        format!(
            "{fixed}{}",
            clip_selected_for_hits(
                value,
                width.saturating_sub(Span::raw(&fixed).width()),
                selected
            )
        )
    };
    let selected_tab_value = selection.and_then(|selection| {
        workspace.tabs.get(selection.tab).map(|tab| {
            let value = if tab.label.is_empty() {
                "unnamed"
            } else {
                tab.label.as_str()
            };
            format!("[{value}]")
        })
    });
    let selected_pane_value = selection.and_then(|selection| {
        selected_tab.and_then(|tab| {
            tab.panes
                .get(selection.pane)
                .map(|pane| format!("[{}]", pane_value(pane)))
        })
    });
    format!(
        "{first}\n{}\n{}\n{}",
        child("tabs", &tabs, selected_tab_value.as_deref()),
        child("panes", &panes, selected_pane_value.as_deref()),
        child(
            detail.split_once(' ').map_or("pane", |(label, _)| label),
            detail.split_once(' ').map_or("unnamed", |(_, value)| value),
            None,
        )
    )
}

pub(crate) fn topology_rows(workspace: &WorkspaceNode, width: usize) -> [String; 4] {
    topology_rows_selected(workspace, width, None)
}

pub(crate) fn topology_rows_selected(
    workspace: &WorkspaceNode,
    width: usize,
    selection: Option<ChildSelection>,
) -> [String; 4] {
    let block = render_workspace_block_selected(workspace, width, selection);
    let mut lines = block.lines().map(str::to_string);
    let mut rows = [
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
        lines.next().unwrap_or_default(),
    ];
    for row in &mut rows {
        let current = Span::raw(row.as_str()).width();
        if current < width {
            row.push_str(&" ".repeat(width - current));
        } else if current > width {
            *row = clip(row, width);
            let used = Span::raw(row.as_str()).width();
            row.push_str(&" ".repeat(width - used));
        }
    }
    rows
}

pub(crate) fn repository_color(
    repository: &str,
    repositories: &[String],
    panel_background: &str,
) -> String {
    const COLORS: [&str; 6] = ["mauve", "teal", "blue", "peach", "green", "red"];
    if repository.is_empty() || COLORS.contains(&panel_background) {
        return "text".into();
    }
    let mut unique = repositories.to_vec();
    unique.sort();
    unique.dedup();
    let hash = repository
        .bytes()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
    let color = COLORS[(hash as usize) % COLORS.len()];
    if color == panel_background || color == "surface0" || color == "Reset" {
        "text".into()
    } else {
        color.into()
    }
}

pub(crate) fn query_entries(topology: &OpenTopology, _include_agents: bool) -> Vec<Entry> {
    let mut entries = Vec::new();
    for workspace in &topology.workspaces {
        let workspace_path = workspace
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.iter())
            .find_map(|pane| (!pane.cwd.as_os_str().is_empty()).then(|| pane.cwd.clone()))
            .unwrap_or_else(|| PathBuf::from(&workspace.label));
        entries.push(Entry {
            source: Source::Workspace,
            title: workspace.label.clone(),
            subtitle: workspace.id.clone(),
            path: workspace_path.clone(),
            workspace_id: Some(workspace.id.clone()),
            workspace_label: Some(workspace.label.clone()),
            agent_target: None,
            project: None,
            action: EntryAction::FocusWorkspace {
                session: workspace.session.clone(),
                id: workspace.id.clone(),
            },
            source_label: None,
            search_terms: vec![workspace.id.clone(), workspace.label.clone()],
        });
        for tab in &workspace.tabs {
            let breadcrumb = format!("{} › {}", workspace.label, tab.label);
            entries.push(Entry {
                source: Source::Workspace,
                title: tab.label.clone(),
                subtitle: breadcrumb.clone(),
                path: PathBuf::from(&breadcrumb),
                workspace_id: Some(workspace.id.clone()),
                workspace_label: Some(workspace.label.clone()),
                agent_target: None,
                project: None,
                action: EntryAction::FocusTab {
                    session: workspace.session.clone(),
                    id: tab.id.clone(),
                },
                source_label: None,
                search_terms: vec![workspace.label.clone(), tab.id.clone(), tab.label.clone()],
            });
            for pane in &tab.panes {
                let key = format!(
                    "pane:{}:{}",
                    workspace.session.as_deref().unwrap_or("<default>"),
                    pane.id
                );
                let breadcrumb = format!(
                    "{} › {} › {}",
                    workspace.label,
                    tab.label,
                    if pane.label.is_empty() {
                        "unnamed"
                    } else {
                        pane.label.as_str()
                    }
                );
                entries.push(Entry {
                    source: Source::Workspace,
                    title: if pane.label.is_empty() {
                        "unnamed".into()
                    } else {
                        pane.label.clone()
                    },
                    subtitle: breadcrumb,
                    path: pane.cwd.clone(),
                    workspace_id: Some(workspace.id.clone()),
                    workspace_label: Some(workspace.label.clone()),
                    agent_target: pane.agent.as_ref().map(|_| pane.id.clone()),
                    project: None,
                    action: EntryAction::FocusPane {
                        session: workspace.session.clone(),
                        id: pane.id.clone(),
                    },
                    source_label: None,
                    search_terms: vec![key, pane.id.clone(), pane.label.clone()],
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
    fn fixture_snapshot_renders_four_line_workspace_block() {
        let snapshot = serde_json::json!({
            "result": {"snapshot": {
                "workspaces": [{"workspace_id":"w1","label":"web-ui","focused":true}],
                "tabs": [{"workspace_id":"w1","tab_id":"w1:t1","label":"Code","focused":true}],
                "panes": [{"workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1","label":"Shell","cwd":"/tmp/web-ui","focused":true}],
                "agents": []
            }}
        });
        let topology = parse_snapshot(&snapshot, None, &[]);
        let rendered = render_workspace_block(&topology.workspaces[0], 120);
        let lines = rendered.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("web-ui"));
        assert!(lines[1].contains("tabs"));
        assert!(lines[2].contains("panes"));
        assert!(lines[3].contains("pane"));
        assert!(lines
            .iter()
            .all(|line| !line.contains('├') && !line.contains('└')));
        assert_eq!(topology.workspaces[0].id, "w1");
        assert_eq!(topology.workspaces[0].tabs[0].panes[0].id, "w1:p1");
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
        let topology = parse_snapshot(&value, Some("work".into()), &[]);
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
    fn aliases_use_first_matching_alias_and_all_states_have_ordered_aggregate() {
        let aliases = [
            AgentAliasConfig {
                alias: "first".into(),
                agent: Some("claude".into()),
                workspace: None,
                path: None,
            },
            AgentAliasConfig {
                alias: "second".into(),
                agent: Some("claude".into()),
                workspace: None,
                path: None,
            },
        ];
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code","focused":true}]),
            serde_json::json!([
                {"workspace_id":"w1","tab_id":"t1","pane_id":"blocked","label":"B","agent":"claude","agent_status":"blocked"},
                {"workspace_id":"w1","tab_id":"t1","pane_id":"done","label":"D","agent":"claude","agent_status":"done"},
                {"workspace_id":"w1","tab_id":"t1","pane_id":"working","label":"W","agent":"claude","agent_status":"working"},
                {"workspace_id":"w1","tab_id":"t1","pane_id":"idle","label":"I","agent":"claude","agent_status":"idle"}
            ]),
            serde_json::json!([]),
        );
        let topology = parse_snapshot(&value, None, &aliases);
        let workspace = &topology.workspaces[0];
        assert_eq!(
            workspace.tabs[0].panes[0]
                .agent
                .as_ref()
                .unwrap()
                .alias
                .as_deref(),
            Some("first")
        );
        assert!(render_workspace_block(workspace, 120)
            .contains("1 blocked · 1 done · 1 working · 1 idle"));
    }

    #[test]
    fn embedded_agent_fields_are_completed_by_exact_pane_join() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1","agent":{"state":"working"}}]),
            serde_json::json!([{"pane_id":"p1","agent":"claude","agent_status":"done"}]),
        );
        let pane = &parse_snapshot(&value, None, &[]).workspaces[0].tabs[0].panes[0];
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
        let pane = &parse_snapshot(&value, None, &[]).workspaces[0].tabs[0].panes[0];
        assert_eq!(pane.agent.as_ref().unwrap().name, "nested");
    }

    #[test]
    fn unknown_agents_are_rendered_only_when_all_detected_agents_are_unknown() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1","agent":"claude","label":"Agent"}]),
            serde_json::json!([]),
        );
        let workspace = &parse_snapshot(&value, None, &[]).workspaces[0];
        assert_eq!(
            workspace.tabs[0].panes[0].agent.as_ref().unwrap().state,
            AgentState::Unknown
        );
        assert!(render_workspace_block(workspace, 120).contains("1 unknown"));
        assert_eq!(AgentState::parse("blocked"), AgentState::Blocked);
        assert_eq!(AgentState::parse("working"), AgentState::Working);
        assert_eq!(AgentState::parse("done"), AgentState::Done);
        assert_eq!(AgentState::parse("idle"), AgentState::Idle);
        assert_eq!(AgentState::parse("unknown"), AgentState::Unknown);
    }

    #[test]
    fn external_empty_terminal_title_is_ordinary_missing_title() {
        let value = snapshot(
            serde_json::json!([{"workspace_id":"w1","label":"Project"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","label":"Code"}]),
            serde_json::json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1"}]),
            serde_json::json!([{"pane_id":"p1","agent":"claude","title":"\u{1b}]0;\u{07}"}]),
        );
        let pane = &parse_snapshot(&value, None, &[]).workspaces[0].tabs[0].panes[0];
        assert_eq!(pane.title, None);
    }

    #[test]
    fn malformed_snapshot_falls_back_and_disabled_agents_skip_collection() {
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
            &[],
            true,
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

        let mut disabled_calls: Vec<Vec<String>> = Vec::new();
        let no_agents = serde_json::json!({"result":{"snapshot":{
            "workspaces":[{"workspace_id":"w2","label":"Project"}],
            "tabs":[{"workspace_id":"w2","tab_id":"t2","label":"Code"}],
            "panes":[{"workspace_id":"w2","tab_id":"t2","pane_id":"p2","label":"Shell"}]
        }}});
        let disabled_topology = collect_topology(
            |args| {
                disabled_calls.push(args.iter().map(|arg| (*arg).to_string()).collect());
                Ok(no_agents.clone())
            },
            None,
            &[],
            false,
        );
        assert_eq!(
            disabled_calls,
            vec![vec!["api", "snapshot"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()]
        );
        assert_eq!(disabled_topology.workspaces[0].id, "w2");
        assert!(disabled_topology.workspaces[0].tabs[0].panes[0]
            .agent
            .is_none());
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
        let mut topology = parse_snapshot(&value, None, &[]);
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
        let mut topology = parse_snapshot(&value, None, &[]);
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
        let mut topology = parse_snapshot(&value, None, &[]);
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
        assert_eq!(
            render_workspace_block(&topology.workspaces[0], 120)
                .lines()
                .next(),
            Some("     Plain")
        );
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
        let entries = query_entries(&parse_snapshot(&value, Some("session".into()), &[]), true);
        assert!(entries.iter().any(|entry| entry.key() == "pane:session:p1"));
        assert!(
            matches!(entries.last().unwrap().action, EntryAction::FocusPane { ref id, .. } if id == "p1")
        );
    }
}
