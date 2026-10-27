use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    activity::{now_unix_secs, parse_agent_status_event, ActivityState},
    config::Config,
    detail::{self, CachedGit, GitDetail, Subagent},
    glance::{self, GlanceRow, Group, Mark, Thresholds},
    herdr::{herdr_json, herdr_json_args, notify_done, notify_error, run_herdr},
    integrations::{command, herdr_plus},
    matcher::match_score,
    model::{workspace_key, Entry, EntryAction, Source, WorkspaceKind, WorkspaceRef},
    paths::{self, canonical_str, herdr_plus_quick_actions_dir, plugin_config_dir},
    recent::RecentState,
    sessions::{self, Session, SessionState, SwitchError, SwitchRequest},
    sources::{collect_open_topology, collect_roots, collect_zoxide},
    theme::Theme,
    topology::{self, AgentState, OpenTopology, WorkspaceNode},
};

/// The footer notice for opening a Workspace in another Herdr session.
pub(crate) const OTHER_SESSION_NOTICE: &str =
    "Start Herdr with helm-herdr shell to switch sessions";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputMode {
    Normal,
    Search,
    Help,
}

#[cfg(test)]
pub(crate) trait HerdrControl {
    fn focus(&self, action: &EntryAction) -> Result<(), String>;
}

pub(crate) struct App {
    pub(crate) config: Config,
    pub(crate) theme: Theme,
    pub(crate) entries: Vec<Entry>,
    pub(crate) filtered: Vec<usize>,
    pub(crate) filtered_scores: Vec<i64>,
    pub(crate) selected: usize,
    pub(crate) query: String,
    pub(crate) input_mode: InputMode,
    pub(crate) source_filter: Option<Source>,
    pub(crate) path_to_workspaces: HashMap<String, Vec<WorkspaceRef>>,
    pub(crate) previous_workspace_id: Option<String>,
    pub(crate) pinned_entries: HashSet<String>,
    /// Workspaces the User sent to Cold, by pin key.
    pub(crate) cold_entries: HashMap<String, i64>,
    pub(crate) recent_state: RecentState,
    pub(crate) spinner_tick: u32,
    pub(crate) update_available: Option<String>,
    pub(crate) topology: OpenTopology,
    /// The Herdr session Helm runs in; Workspaces of any other session are elsewhere.
    pub(crate) session: Option<String>,
    /// Every discovered Herdr session, the current one first.
    pub(crate) sessions: Vec<SessionState>,
    /// Where `helm-herdr shell` launchers keep their records and switch
    /// requests. `None` until the caller sets it, which disables switching.
    pub(crate) launchers_dir: Option<PathBuf>,
    /// The Sessions view order as group and session name. A refresh keeps it,
    /// so rows and their labels stay put while the Picker is open.
    session_ranks: Vec<(usize, Option<String>)>,
    /// Git reads of other sessions' Workspaces for the Sessions view, keyed by
    /// session label, with the time of the read.
    pub(crate) session_git: HashMap<String, (OpenTopology, i64)>,
    pub(crate) topology_entries: Vec<Entry>,
    pub(crate) activity: ActivityState,
    pub(crate) glance: HashMap<String, GlanceRow>,
    /// Position of each glance row within the glance, keyed by workspace key.
    /// A refresh keeps it, so rows and their labels stay put while the Picker is open.
    row_ranks: HashMap<String, (Group, usize)>,
    pub(crate) now: i64,
    pub(crate) detail_scroll: DetailScroll,
    pub(crate) last_label: Option<char>,
    pub(crate) close_armed: bool,
    pub(crate) cold_armed: bool,
    /// A footer message that the next key clears.
    pub(crate) notice: Option<String>,
    /// Largest useful detail offset, set by the last render.
    pub(crate) detail_max_offset: usize,
    /// Running Claude subagents, keyed by pane id.
    pub(crate) subagents: HashMap<String, Vec<Subagent>>,
    /// Git reads of the glance workspaces, keyed by workspace key.
    pub(crate) git_detail: HashMap<String, CachedGit>,
    #[cfg(test)]
    pub(crate) herdr_control: Option<Box<dyn HerdrControl>>,
    initial_selection_pending: bool,
    /// The row a glance search selected on its own. While the selection stays
    /// there, it follows the best match as the Query changes.
    search_pick: Option<String>,
}

/// Scroll offset of the detail pane. It belongs to one entry, so it resets
/// once the selection moves to another.
#[derive(Clone, Debug, Default)]
pub(crate) struct DetailScroll {
    pub(crate) entry: Option<String>,
    pub(crate) offset: usize,
}

impl App {
    pub(crate) fn new(config: Config, theme: Theme) -> Self {
        Self {
            config,
            theme,
            entries: vec![],
            filtered: vec![],
            filtered_scores: vec![],
            selected: 0,
            query: String::new(),
            input_mode: InputMode::Normal,
            source_filter: None,
            path_to_workspaces: HashMap::new(),
            previous_workspace_id: None,
            pinned_entries: HashSet::new(),
            cold_entries: HashMap::new(),
            recent_state: RecentState::default(),
            spinner_tick: 0,
            update_available: None,
            topology: OpenTopology::default(),
            session: None,
            sessions: Vec::new(),
            launchers_dir: None,
            session_ranks: Vec::new(),
            session_git: HashMap::new(),
            topology_entries: Vec::new(),
            activity: ActivityState::default(),
            glance: HashMap::new(),
            row_ranks: HashMap::new(),
            now: 0,
            detail_scroll: DetailScroll::default(),
            last_label: None,
            close_armed: false,
            cold_armed: false,
            notice: None,
            detail_max_offset: 0,
            subagents: HashMap::new(),
            git_detail: HashMap::new(),
            #[cfg(test)]
            herdr_control: None,
            initial_selection_pending: true,
            search_pick: None,
        }
    }

    pub(crate) fn refresh(&mut self) {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        let (open_entries, path_to_workspaces, snapshot) =
            collect_open_topology(self.config.sources.open_workspaces);
        self.path_to_workspaces = path_to_workspaces;
        let mut workspaces = load_workspaces(&self.config, None, snapshot.as_ref());
        self.recent_state = RecentState::load();
        if self.config.sources.open_workspaces {
            normalize_recent_state(&mut self.recent_state, &open_entries);
            self.recent_state.save();
        }
        if let Some(activity) = prune_activity(&workspaces.local_topology()) {
            workspaces.activity = activity;
        }

        if self.config.sources.zoxide {
            push_unique(&mut entries, &mut seen, collect_zoxide());
        }
        if self.config.sources.roots {
            push_unique(&mut entries, &mut seen, collect_roots(&self.config));
        }
        if self.config.sources.herdr_plus_quick_actions && herdr_plus_quick_actions_dir().is_dir() {
            entries.push(herdr_plus::quick_actions_entry());
        }
        push_unique(
            &mut entries,
            &mut seen,
            command::collect(&self.config.integrations),
        );

        self.entries = entries;
        self.pinned_entries = read_entry_keys(&plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE))
            .unwrap_or_default();
        self.cold_entries = read_cold_entries(&plugin_config_dir().join(COLD_ENTRIES_STATE_FILE))
            .unwrap_or_default();
        self.session_ranks.clear();
        self.set_workspace_state(workspaces, now_unix_secs());
        self.row_ranks.clear();
        if migrate_legacy_topology_pins(&self.entries, &mut self.pinned_entries) {
            let _ = save_entry_keys(
                &plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE),
                &self.pinned_entries,
            );
        }
        self.rebuild_glance();
        self.apply_filter();
    }

    /// Replaces the live workspaces from a background reload and keeps the
    /// selected entry on the same destination.
    pub(crate) fn apply_workspaces(&mut self, workspaces: Workspaces, now: i64) {
        // A failed Herdr read comes back empty. Keep the last good list instead.
        let has_local = |topology: &OpenTopology, session: &Option<String>| {
            local_workspaces(topology, session).next().is_some()
        };
        if !has_local(&workspaces.topology, &workspaces.session)
            && has_local(&self.topology, &self.session)
        {
            return;
        }
        let selected = self.selected_entry().map(entry_identity);
        self.set_workspace_state(workspaces, now);
        self.rebuild_glance();
        self.filter_entries(selected.clone());
        if let Some(selected) = selected {
            if let Some(position) = self
                .filtered
                .iter()
                .position(|index| entry_identity(&self.entries[*index]) == selected)
            {
                self.selected = position;
            }
        }
    }

    fn set_workspace_state(&mut self, workspaces: Workspaces, now: i64) {
        self.topology = workspaces.topology;
        self.session = workspaces.session;
        self.activity = workspaces.activity;
        self.previous_workspace_id = workspaces.previous;
        self.subagents = workspaces.subagents;
        self.now = now;
        self.sessions = workspaces.sessions;
        let rest: Vec<WorkspaceNode> = sessions::other_topology(&self.sessions)
            .workspaces
            .into_iter()
            .filter(|workspace| {
                !self
                    .topology
                    .workspaces
                    .iter()
                    .any(|known| known.id == workspace.id && known.session == workspace.session)
            })
            .collect();
        self.topology.workspaces.extend(rest);
        self.topology_entries = topology::query_entries(&self.topology);
        self.entries
            .retain(|entry| !matches!(entry.source, Source::Workspace | Source::Session));
        self.entries.extend(self.topology_entries.iter().cloned());
        let sessions = self.session_entries();
        self.entries.extend(sessions);
    }

    pub(crate) fn rebuild_glance(&mut self) {
        let thresholds = Thresholds {
            hot_secs: self.config.glance.hot_minutes * 60,
            warm_secs: self.config.glance.warm_hours * 3_600,
        };
        self.glance = self
            .topology
            .workspaces
            .iter()
            .map(|workspace| {
                let elsewhere = workspace.session != self.session;
                let entry = self
                    .topology_entries
                    .iter()
                    .find(|entry| {
                        matches!(&entry.action, EntryAction::FocusWorkspace { session, id } if id == &workspace.id && session == &workspace.session)
                    })
                    .filter(|_| !elsewhere);
                let pinned = entry.is_some_and(|entry| self.is_pinned(entry));
                let sent_cold_at =
                    entry.and_then(|entry| self.cold_entries.get(&pin_key(entry)).copied());
                let evidence = glance::evidence(
                    workspace,
                    &self.activity,
                    pinned,
                    sent_cold_at,
                    elsewhere,
                );
                let previous = !elsewhere
                    && self.previous_workspace_id.as_deref() == Some(workspace.id.as_str());
                let mut row = glance::row(workspace, &evidence, previous, self.now, thresholds);
                if elsewhere {
                    row.mark = Mark::None;
                    row.other_session =
                        Some(Session::label(workspace.session.as_deref()).to_string());
                }
                (
                    workspace_key(workspace.session.as_deref(), &workspace.id),
                    row,
                )
            })
            .collect();
        self.rank_rows();
    }

    /// Orders the glance rows by group, then by newest activity. Rows ranked
    /// before keep their place while they stay in their group; new rows and
    /// rows that changed group follow at the end of their group.
    fn rank_rows(&mut self) {
        let mut fresh: Vec<(Group, Option<i64>, String)> = self
            .topology
            .workspaces
            .iter()
            .filter_map(|workspace| {
                let key = workspace_key(workspace.session.as_deref(), &workspace.id);
                let row = self.glance.get(&key)?;
                Some((row.group, row.last_active, key))
            })
            .collect();
        fresh.sort_by_key(|(group, last_active, _)| (*group, Reverse(*last_active)));
        let mut ranked: Vec<(Group, (bool, usize), String)> = fresh
            .into_iter()
            .enumerate()
            .map(|(fresh_rank, (group, _, key))| {
                let kept = self
                    .row_ranks
                    .get(&key)
                    .filter(|(old_group, _)| *old_group == group)
                    .map(|(_, rank)| *rank);
                (
                    group,
                    kept.map_or((true, fresh_rank), |rank| (false, rank)),
                    key,
                )
            })
            .collect();
        ranked.sort();
        self.row_ranks = ranked
            .into_iter()
            .enumerate()
            .map(|(rank, (group, _, key))| (key, (group, rank)))
            .collect();
    }

    pub(crate) fn glance_view(&self) -> bool {
        self.source_filter
            .as_ref()
            .is_none_or(|source| *source == Source::Workspace)
    }

    pub(crate) fn sessions_view(&self) -> bool {
        self.source_filter == Some(Source::Session)
    }

    /// One Entry per session: the current one, the other running ones by
    /// newest activity, then the stopped ones, each in discovery order
    /// otherwise. Sessions ranked before keep their place while they stay
    /// running or stopped; the others follow at the end of their group.
    fn session_entries(&mut self) -> Vec<Entry> {
        let group = |state: &SessionState| match (state.current, state.topology.is_some()) {
            (true, _) => 0,
            (false, true) => 1,
            (false, false) => 2,
        };
        let mut fresh: Vec<(usize, Reverse<Option<i64>>, &SessionState)> = self
            .sessions
            .iter()
            .map(|state| {
                let last_active = match group(state) {
                    1 => self.session_summary(state).last_active,
                    _ => None,
                };
                (group(state), Reverse(last_active), state)
            })
            .collect();
        fresh.sort_by_key(|(group, last_active, _)| (*group, *last_active));
        let mut ranked: Vec<(usize, (bool, usize), &SessionState)> = fresh
            .into_iter()
            .enumerate()
            .map(|(fresh_rank, (group, _, state))| {
                let kept = self.session_ranks.iter().position(|(old_group, name)| {
                    *old_group == group && name == &state.session.name
                });
                (
                    group,
                    kept.map_or((true, fresh_rank), |rank| (false, rank)),
                    state,
                )
            })
            .collect();
        ranked.sort_by_key(|(group, rank, _)| (*group, *rank));
        let entries = ranked
            .iter()
            .map(|(_, _, state)| Entry {
                source: Source::Session,
                title: Session::label(state.session.name.as_deref()).to_string(),
                subtitle: String::new(),
                path: state.session.dir.clone(),
                workspace_id: None,
                workspace_label: None,
                agent_target: None,
                action: EntryAction::SwitchSession {
                    session: state.session.name.clone(),
                    workspace: None,
                    tab: None,
                },
                source_label: None,
                search_terms: Vec::new(),
            })
            .collect();
        self.session_ranks = ranked
            .into_iter()
            .map(|(group, _, state)| (group, state.session.name.clone()))
            .collect();
        entries
    }

    /// The session of a Sessions view row.
    pub(crate) fn session_state(&self, entry: &Entry) -> Option<&SessionState> {
        let EntryAction::SwitchSession {
            session,
            workspace: None,
            ..
        } = &entry.action
        else {
            return None;
        };
        self.sessions
            .iter()
            .find(|state| &state.session.name == session)
    }

    /// A session's Workspaces in the Sessions view order: by Warmth group, then
    /// newest activity first. Pinned never applies, and Cold by hand only in
    /// the current session. Other sessions get git from the Sessions view's
    /// reads or from their Needs you rows, when either has it.
    pub(crate) fn session_workspaces(
        &self,
        state: &SessionState,
    ) -> Vec<(GlanceRow, WorkspaceNode)> {
        let thresholds = Thresholds {
            hot_secs: self.config.glance.hot_minutes * 60,
            warm_secs: self.config.glance.warm_hours * 3_600,
        };
        let workspaces: Vec<WorkspaceNode> = if state.current {
            self.local_workspaces().cloned().collect()
        } else {
            let read = self
                .session_git
                .get(Session::label(state.session.name.as_deref()));
            let mut workspaces = state.topology.clone().unwrap_or_default().workspaces;
            for workspace in &mut workspaces {
                workspace.git = read
                    .into_iter()
                    .flat_map(|(topology, _)| &topology.workspaces)
                    .chain(&self.topology.workspaces)
                    .find(|known| known.id == workspace.id && known.session == workspace.session)
                    .and_then(|known| known.git.clone());
            }
            workspaces
        };
        let mut rows: Vec<(GlanceRow, WorkspaceNode)> = workspaces
            .into_iter()
            .map(|workspace| {
                let sent_cold_at = self
                    .cold_entries
                    .get(&topology_workspace_pin_key(
                        workspace.session.as_deref(),
                        &workspace.id,
                    ))
                    .copied()
                    .filter(|_| state.current);
                let evidence = glance::evidence(
                    &workspace,
                    &self.activity,
                    false,
                    sent_cold_at,
                    !state.current,
                );
                let previous = state.current
                    && self.previous_workspace_id.as_deref() == Some(workspace.id.as_str());
                let mut row = glance::row(&workspace, &evidence, previous, self.now, thresholds);
                if !state.current {
                    row.mark = Mark::None;
                }
                (row, workspace)
            })
            .collect();
        rows.sort_by_key(|(row, _)| (row.group, Reverse(row.last_active)));
        rows
    }

    /// What a Sessions view row shows about its session.
    pub(crate) fn session_summary(&self, state: &SessionState) -> SessionSummary {
        let rows = self.session_workspaces(state);
        let waiting: Vec<AgentState> = rows
            .iter()
            .filter(|(row, _)| row.needs_you)
            .map(|(row, _)| row.state)
            .collect();
        let focused = self.last_focused(rows.iter().map(|(_, workspace)| workspace));
        let agent_change = rows
            .iter()
            .flat_map(|(_, workspace)| {
                workspace
                    .tabs
                    .iter()
                    .flat_map(|tab| &tab.panes)
                    .filter_map(|pane| {
                        self.activity
                            .pane(workspace.session.as_deref(), &pane.id)
                            .map(|recorded| recorded.since)
                    })
            })
            .max();
        SessionSummary {
            state: glance::most_urgent(rows.iter().map(|(row, _)| row.state)).filter(|state| {
                matches!(
                    state,
                    AgentState::Blocked | AgentState::Done | AgentState::Working
                )
            }),
            needs_you: glance::most_urgent(waiting.iter().copied())
                .map(|state| (state, waiting.len())),
            workspaces: rows.len(),
            last_active: focused.map(|(at, _)| at).max(agent_change),
            last_focused: focused.map(|(_, workspace)| workspace.label.clone()),
        }
    }

    /// The Workspace focused most recently according to `activity.json`, with its focus time.
    fn last_focused<'a>(
        &self,
        workspaces: impl Iterator<Item = &'a WorkspaceNode>,
    ) -> Option<(i64, &'a WorkspaceNode)> {
        workspaces
            .filter_map(|workspace| {
                let at = self
                    .activity
                    .focused_at(workspace.session.as_deref(), &workspace.id)?;
                Some((at, workspace))
            })
            .max_by_key(|(at, _)| *at)
    }

    /// The target of `l` when it lies in another session, as session and
    /// Workspace ID: the Workspace focused most recently across every running
    /// session, when that is not a Workspace of this session. The current
    /// Workspace never counts.
    pub(crate) fn previous_elsewhere(&self) -> Option<(Option<String>, String)> {
        let local = self.last_focused(
            self.local_workspaces()
                .filter(|workspace| !workspace.focused),
        );
        let (at, workspace, session) = self
            .sessions
            .iter()
            .filter(|state| !state.current)
            .filter_map(|state| {
                let topology = state.topology.as_ref()?;
                let (at, workspace) = self.last_focused(topology.workspaces.iter())?;
                Some((at, workspace, &state.session.name))
            })
            .max_by_key(|(at, _, _)| *at)?;
        if local.is_some_and(|(local_at, _)| local_at >= at) {
            return None;
        }
        Some((session.clone(), workspace.id.clone()))
    }

    /// What opening the selected Sessions view row does. `number` picks the
    /// session's Workspace in detail order. A Workspace of the current session
    /// opens here; any other session switches, at that Workspace, or without a
    /// number at its last focused Workspace when one is known. `None` for the
    /// current session's own row or a missing numbered Workspace.
    pub(crate) fn session_action(&self, number: Option<usize>) -> Option<EntryAction> {
        let state = self.session_state(self.selected_entry()?)?;
        let rows = self.session_workspaces(state);
        let workspace = match number {
            Some(number) => Some(&rows.get(number.checked_sub(1)?)?.1),
            None if state.current => None,
            None => self
                .last_focused(rows.iter().map(|(_, workspace)| workspace))
                .map(|(_, workspace)| workspace),
        }
        .cloned();
        if state.current {
            let workspace = workspace?;
            return Some(EntryAction::FocusWorkspace {
                session: workspace.session,
                id: workspace.id,
            });
        }
        Some(EntryAction::SwitchSession {
            session: state.session.name.clone(),
            workspace: workspace.map(|workspace| workspace.id),
            tab: None,
        })
    }

    /// Switches to `session` (`None` is the default session), at `workspace`,
    /// `tab` and `pane` when given, through the launcher that runs this session's
    /// client. A launcher that fails to switch leaves a footer notice.
    pub(crate) fn switch_session(
        &mut self,
        session: Option<&str>,
        workspace: Option<String>,
        tab: Option<String>,
        pane: Option<String>,
    ) -> Result<(), SwitchError> {
        let label = Session::label(session).to_string();
        let request = SwitchRequest {
            session: label.clone(),
            workspace_id: workspace,
            tab_id: tab,
            pane_id: pane,
        };
        let dir = self
            .launchers_dir
            .as_deref()
            .ok_or(SwitchError::NoLauncher)?;
        let result =
            sessions::request_switch_at(dir, Session::label(self.session.as_deref()), &request);
        if let Err(SwitchError::Failed(_)) = result {
            self.notice = Some(format!("Switching to {label} failed"));
        }
        result
    }

    /// [`App::switch_session`], with the start-the-launcher notice when no
    /// launcher runs. Returns whether the switch went out.
    pub(crate) fn switch_or_notice(
        &mut self,
        session: Option<&str>,
        workspace: Option<String>,
        tab: Option<String>,
        pane: Option<String>,
    ) -> bool {
        match self.switch_session(session, workspace, tab, pane) {
            Ok(()) => true,
            Err(SwitchError::NoLauncher) => {
                self.notice = Some(OTHER_SESSION_NOTICE.to_string());
                false
            }
            Err(SwitchError::Failed(_)) => false,
        }
    }

    /// This session's own Workspaces in the live Topology.
    pub(crate) fn local_workspaces(&self) -> impl Iterator<Item = &WorkspaceNode> {
        local_workspaces(&self.topology, &self.session)
    }

    /// The session tag of a glance row from another Herdr session.
    pub(crate) fn elsewhere_session(&self, entry: &Entry) -> Option<&str> {
        self.glance_row(entry)?.other_session.as_deref()
    }

    /// The selected running other session, with its Workspaces, when the
    /// Sessions view's git read for it is missing or older than its TTL.
    pub(crate) fn session_git_target(&self, now: i64) -> Option<(String, OpenTopology)> {
        if !self.sessions_view() {
            return None;
        }
        let state = self.session_state(self.selected_entry()?)?;
        let topology = state.topology.clone().filter(|_| !state.current)?;
        let label = Session::label(state.session.name.as_deref()).to_string();
        self.session_git
            .get(&label)
            .is_none_or(|(_, read_at)| now - read_at >= SESSION_GIT_TTL_SECS)
            .then_some((label, topology))
    }

    pub(crate) fn glance_row(&self, entry: &Entry) -> Option<&GlanceRow> {
        match (&entry.source, &entry.action) {
            (Source::Workspace, EntryAction::FocusWorkspace { session, id }) => {
                self.glance.get(&workspace_key(session.as_deref(), id))
            }
            _ => None,
        }
    }

    /// The glance group of an entry shown in the glance view. Directories are
    /// only shown while searching, so every other Source returns `None`.
    pub(crate) fn glance_group(&self, entry: &Entry) -> Option<Group> {
        match entry.source {
            Source::Zoxide | Source::Root => Some(Group::Directories),
            Source::QuickAction | Source::Integration => Some(Group::Other),
            _ => self.glance_row(entry).map(|row| row.group),
        }
    }

    fn glance_order(&self, entry: &Entry, score: i64) -> (Group, usize, Reverse<i64>) {
        match self.row_ranks.get(&entry_identity(entry)) {
            Some((group, rank)) => (*group, *rank, Reverse(0)),
            None => (
                self.glance_group(entry).unwrap_or(Group::Directories),
                0,
                Reverse(score),
            ),
        }
    }

    /// Re-runs the Query. In a glance search the selection follows the best
    /// match until the user moves it, then stays on that row while it is visible.
    pub(crate) fn apply_filter(&mut self) {
        let current = self.selected_entry().map(entry_identity);
        let moved = self.search_pick.is_some() && current != self.search_pick;
        self.filter_entries(current.filter(|_| moved));
    }

    /// Filters and orders the entries. `keep` names an entry to keep selected
    /// in a glance search while it stays visible.
    fn filter_entries(&mut self, keep: Option<String>) {
        let query = Query::parse(&self.query);
        let searching = !self.query.trim().is_empty();
        let glance = self.glance_view();
        let sessions = self.sessions_view();

        for entry in &mut self.entries {
            if entry.source_name() == "bookmark" {
                entry.source_label = Some(ordinary_source_label(entry).into());
            }
            if self.pinned_entries.contains(&pin_key(entry)) {
                entry.source_label = Some("bookmark".into());
            }
        }

        let mut scored = Vec::new();
        for (idx, entry) in self.entries.iter().enumerate() {
            if let Some(source) = &self.source_filter {
                if &entry.source != source {
                    continue;
                }
            }
            if glance {
                match self.glance_group(entry) {
                    Some(Group::Directories | Group::Other) if !searching => continue,
                    Some(_) => {}
                    None => continue,
                }
                if !query.all_sessions && self.glance_row(entry).is_some_and(GlanceRow::search_only)
                {
                    continue;
                }
            }
            if !query.filters_match(entry) {
                continue;
            }
            let bonus = self.config.picker.source_bonus(&entry.source);
            if query.plain.is_empty() {
                scored.push((0, bonus, idx));
            } else if let Some(score) = match_score(
                &self.config.picker.engine,
                &self.search_text(entry),
                &query.plain,
            ) {
                scored.push((score, bonus, idx));
            }
        }
        if glance && !query.plain.is_empty() && self.config.picker.engine_is_nucleo() {
            let best = scored.iter().map(|(score, ..)| *score).max().unwrap_or(0);
            scored.retain(|(score, ..)| near_best(*score, best));
        }
        let mut scored: Vec<(i64, usize)> = scored
            .into_iter()
            .map(|(score, bonus, idx)| (score + bonus, idx))
            .collect();
        if glance {
            scored.sort_by_key(|(score, idx)| self.glance_order(&self.entries[*idx], *score));
        } else if sessions {
            scored.sort_by_key(|(_, idx)| *idx);
        } else {
            scored.sort_by(|(score_a, idx_a), (score_b, idx_b)| {
                let user_pinned_a = self.is_pinned(&self.entries[*idx_a]);
                let user_pinned_b = self.is_pinned(&self.entries[*idx_b]);
                user_pinned_b
                    .cmp(&user_pinned_a)
                    .then_with(|| score_b.cmp(score_a))
                    .then_with(|| {
                        self.config
                            .picker
                            .source_rank(&self.entries[*idx_a].source)
                            .cmp(&self.config.picker.source_rank(&self.entries[*idx_b].source))
                    })
                    .then_with(|| idx_a.cmp(idx_b))
            });
        }

        self.filtered = Vec::new();
        self.filtered_scores = Vec::new();
        for (score, index) in scored {
            self.filtered.push(index);
            self.filtered_scores.push(score);
        }
        self.selected = if searching {
            self.best_search_position(&query).unwrap_or(0)
        } else {
            0
        };
        let pick = self.search_pick.take();
        if searching && glance {
            match keep.and_then(|keep| {
                self.filtered
                    .iter()
                    .position(|index| entry_identity(&self.entries[*index]) == keep)
            }) {
                Some(position) => {
                    self.selected = position;
                    self.search_pick = pick;
                }
                None => self.search_pick = self.selected_entry().map(entry_identity),
            }
        }
        if self.initial_selection_pending {
            self.initial_selection_pending = false;
            if self.query.trim().is_empty() && self.source_filter.is_none() {
                let here = local_workspaces(&self.topology, &self.session)
                    .find(|workspace| workspace.focused)
                    .map(|workspace| workspace.id.as_str());
                let workspace_id =
                    |position: &usize| match &self.entries[self.filtered[*position]].action {
                        EntryAction::FocusWorkspace { id, .. } => Some(id.as_str()),
                        _ => None,
                    };
                let positions = 0..self.filtered.len();
                if let Some(position) = positions
                    .clone()
                    .find(|position| here.is_some() && workspace_id(position) == here)
                    .or_else(|| {
                        positions
                            .clone()
                            .find(|position| workspace_id(position).is_some())
                    })
                {
                    self.selected = position;
                }
            }
        }
    }

    fn best_search_position(&self, query: &Query) -> Option<usize> {
        self.filtered
            .iter()
            .enumerate()
            .filter_map(|(position, index)| {
                let entry = &self.entries[*index];
                if !query.filters_match(entry) {
                    return None;
                }
                let score = if query.plain.is_empty() {
                    0
                } else {
                    match_score(
                        &self.config.picker.engine,
                        &self.search_text(entry),
                        &query.plain,
                    )?
                };
                let score = score + self.config.picker.source_bonus(&entry.source);
                Some((score, position))
            })
            .max_by(|(score_a, position_a), (score_b, position_b)| {
                score_a
                    .cmp(score_b)
                    .then_with(|| position_b.cmp(position_a))
            })
            .map(|(_, position)| position)
    }

    /// The text a Query matches: a Sessions view row matches by session name.
    fn search_text(&self, entry: &Entry) -> String {
        if entry.source == Source::Session {
            entry.title.to_lowercase()
        } else {
            entry.haystack()
        }
    }

    /// Drop the trailing word of the query, plus any whitespace before it.
    pub(crate) fn delete_query_word(&mut self) {
        let trimmed = self.query.trim_end();
        let cut = trimmed
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);
        self.query.truncate(cut);
    }

    pub(crate) fn set_filter(&mut self, source: Option<Source>) {
        if source
            .as_ref()
            .is_some_and(|source| !self.config.sources.enabled(source))
        {
            return;
        }
        self.source_filter = if self.source_filter == source {
            None
        } else {
            source
        };
        self.selected = 0;
        self.search_pick = None;
    }

    /// Steps through the sources of the filter strip in its order, wrapping at either end.
    pub(crate) fn cycle_filter(&mut self, forward: bool) {
        let sources: Vec<_> = self
            .config
            .enabled_sources_in_order()
            .into_iter()
            .filter(|source| self.config.picker.filter_key(source).is_some())
            .collect();
        let current = self.source_filter.clone().unwrap_or(Source::Workspace);
        let Some(index) = sources.iter().position(|source| *source == current) else {
            return;
        };
        let step = if forward { 1 } else { sources.len() - 1 };
        let next = sources[(index + step) % sources.len()].clone();
        if next != current {
            self.set_filter((next != Source::Workspace).then_some(next));
        }
    }

    fn glance_row_at(&self, position: usize) -> Option<&GlanceRow> {
        let index = *self.filtered.get(position)?;
        self.glance_row(&self.entries[index])
    }

    fn group_at(&self, position: usize) -> Option<Group> {
        let index = *self.filtered.get(position)?;
        self.glance_group(&self.entries[index])
    }

    /// Positions of the labelled rows, in label order: the first visible
    /// glance rows, Pinned first, or the first Sessions view rows.
    pub(crate) fn labelled_positions(&self) -> Vec<usize> {
        let sessions = self.sessions_view();
        if !self.glance_view() && !sessions {
            return Vec::new();
        }
        let count = self.config.glance.labels().len();
        (0..self.filtered.len())
            .filter(|&position| sessions || self.glance_row_at(position).is_some())
            .take(count)
            .collect()
    }

    /// Selects the first row of the next or previous Warmth group.
    pub(crate) fn jump_group(&mut self, forward: bool) {
        if !self.glance_view() {
            return;
        }
        let groups: Vec<Option<Group>> = (0..self.filtered.len())
            .map(|position| self.group_at(position))
            .collect();
        let starts: Vec<usize> = (0..groups.len())
            .filter(|&position| {
                groups[position].is_some()
                    && (position == 0 || groups[position - 1] != groups[position])
            })
            .collect();
        let target = if forward {
            starts.iter().copied().find(|&start| start > self.selected)
        } else {
            // From inside a group, go to its first row; from that row, go to the previous group's.
            let current = starts
                .iter()
                .rev()
                .copied()
                .find(|&start| start <= self.selected);
            match current {
                Some(current) if current < self.selected => Some(current),
                Some(current) => starts.iter().rev().copied().find(|&start| start < current),
                None => None,
            }
        };
        if let Some(target) = target {
            self.selected = target;
        }
    }

    /// Selects the next row that needs you, wrapping to the first.
    pub(crate) fn jump_needs_you(&mut self) {
        let rows: Vec<usize> = (0..self.filtered.len())
            .filter(|&position| {
                self.glance_row_at(position)
                    .is_some_and(|row| row.needs_you)
            })
            .collect();
        let target = rows
            .iter()
            .copied()
            .find(|&position| position > self.selected)
            .or_else(|| rows.first().copied());
        if let Some(target) = target {
            self.selected = target;
        }
    }

    /// The row of the Previous workspace, when it is listed.
    pub(crate) fn previous_position(&self) -> Option<usize> {
        (0..self.filtered.len()).find(|&position| {
            self.glance_row_at(position)
                .is_some_and(|row| row.mark == Mark::Last)
        })
    }

    /// The entry action for tab `number` (1-based) of the selected Workspace.
    pub(crate) fn selected_tab_action(&self, number: usize) -> Option<EntryAction> {
        let EntryAction::FocusWorkspace { session, id } = &self.selected_entry()?.action else {
            return None;
        };
        let workspace = self
            .topology
            .workspaces
            .iter()
            .find(|workspace| &workspace.id == id && &workspace.session == session)?;
        let tab = workspace.tabs.get(number.checked_sub(1)?)?;
        Some(EntryAction::FocusTab {
            session: session.clone(),
            id: tab.id.clone(),
        })
    }

    pub(crate) fn scroll_detail(&mut self, delta: isize) {
        self.sync_detail_scroll();
        let offset = self.detail_scroll.offset.saturating_add_signed(delta);
        self.detail_scroll.offset = offset.min(self.detail_max_offset);
    }

    pub(crate) fn workspace_node(&self, entry: &Entry) -> Option<&topology::WorkspaceNode> {
        let EntryAction::FocusWorkspace { session, id } = &entry.action else {
            return None;
        };
        self.topology
            .workspaces
            .iter()
            .find(|workspace| &workspace.id == id && &workspace.session == session)
    }

    /// The selected glance workspace, resolved once for rendering and git reads.
    pub(crate) fn selected_workspace(&self) -> Option<SelectedWorkspace<'_>> {
        let entry = self.selected_entry()?;
        let row = self.glance_row(entry)?;
        let workspace = self.workspace_node(entry)?;
        let EntryAction::FocusWorkspace { session, id } = &entry.action else {
            return None;
        };
        Some(SelectedWorkspace {
            entry,
            workspace,
            row,
            key: workspace_key(session.as_deref(), id),
        })
    }

    /// The git cache key and working directory of the selected glance workspace.
    pub(crate) fn detail_target(&self) -> Option<(String, PathBuf)> {
        let selected = self.selected_workspace()?;
        (!selected.entry.path.as_os_str().is_empty())
            .then(|| (selected.key, selected.entry.path.clone()))
    }

    /// Whether a git read is missing or older than the detail TTL.
    pub(crate) fn git_refresh_due(&self, key: &str, now: i64) -> bool {
        self.git_detail
            .get(key)
            .is_none_or(|cached| now - cached.fetched_at >= DETAIL_TTL_SECS)
    }

    pub(crate) fn cache_git(&mut self, key: String, detail: GitDetail, now: i64) {
        self.git_detail.insert(
            key,
            CachedGit {
                detail,
                fetched_at: now,
            },
        );
    }

    /// Summed progress of the running subagents in a workspace.
    pub(crate) fn subagent_progress(
        &self,
        workspace: &topology::WorkspaceNode,
    ) -> Option<(u32, u32)> {
        let (done, total) = workspace
            .tabs
            .iter()
            .flat_map(|tab| &tab.panes)
            .filter_map(|pane| {
                self.subagents
                    .get(&workspace_key(workspace.session.as_deref(), &pane.id))
            })
            .flatten()
            .fold((0, 0), |(done, total), subagent| {
                (done + subagent.done, total + subagent.total)
            });
        (total > 0).then_some((done, total))
    }

    /// Resets the detail offset when the selection has moved to another entry.
    pub(crate) fn sync_detail_scroll(&mut self) {
        let entry = self.selected_entry().map(entry_identity);
        if self.detail_scroll.entry != entry {
            self.detail_scroll = DetailScroll { entry, offset: 0 };
        }
    }

    pub(crate) fn next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
        }
    }
    pub(crate) fn prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
    pub(crate) fn selected_entry(&self) -> Option<&Entry> {
        self.filtered
            .get(self.selected)
            .and_then(|idx| self.entries.get(*idx))
    }

    pub(crate) fn is_pinned(&self, entry: &Entry) -> bool {
        self.pinned_entries.contains(&pin_key(entry))
            || legacy_current_workspace_pin_key(entry)
                .is_some_and(|key| self.pinned_entries.contains(&key))
    }

    pub(crate) fn toggle_selected_pin(&mut self) -> Result<(), String> {
        let entry = self.selected_entry().ok_or("nothing selected")?;
        let key = pin_key(entry);
        let legacy_key = legacy_current_workspace_pin_key(entry);
        let mut pinned = self.pinned_entries.clone();
        let removed_primary = pinned.remove(&key);
        let removed_legacy = legacy_key
            .as_ref()
            .is_some_and(|legacy| pinned.remove(legacy));
        let mut cold = self.cold_entries.clone();
        if !(removed_primary || removed_legacy) {
            cold.remove(&key);
            pinned.insert(key);
        }
        save_entry_keys(
            &plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE),
            &pinned,
        )?;
        save_cold_entries(&plugin_config_dir().join(COLD_ENTRIES_STATE_FILE), &cold)?;
        self.pinned_entries = pinned;
        self.cold_entries = cold;
        self.rebuild_glance();
        self.apply_filter();
        Ok(())
    }

    /// Whether the entry is the glance row of a Workspace in another Herdr session.
    pub(crate) fn is_elsewhere(&self, entry: &Entry) -> bool {
        self.glance_row(entry)
            .is_some_and(|row| row.other_session.is_some())
    }

    pub(crate) fn is_cold(&self, entry: &Entry) -> bool {
        self.glance_row(entry).is_some_and(|row| row.cold)
    }

    /// Sends the selected Workspace to Cold, or lets it warm up again.
    /// Going cold unpins it.
    pub(crate) fn toggle_selected_cold(&mut self) -> Result<(), String> {
        let entry = self.selected_entry().ok_or("nothing selected")?;
        if !matches!(entry.action, EntryAction::FocusWorkspace { .. }) {
            return Err("only a workspace can go cold".into());
        }
        let key = pin_key(entry);
        let legacy_key = legacy_current_workspace_pin_key(entry);
        let mut cold = self.cold_entries.clone();
        let mut pinned = self.pinned_entries.clone();
        if self.is_cold(entry) {
            cold.remove(&key);
        } else {
            pinned.remove(&key);
            if let Some(legacy) = &legacy_key {
                pinned.remove(legacy);
            }
            cold.insert(key, now_unix_secs());
        }
        save_cold_entries(&plugin_config_dir().join(COLD_ENTRIES_STATE_FILE), &cold)?;
        save_entry_keys(
            &plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE),
            &pinned,
        )?;
        self.cold_entries = cold;
        self.pinned_entries = pinned;
        self.rebuild_glance();
        self.apply_filter();
        Ok(())
    }

    /// Clears the done state of the selected Workspace's agents until one
    /// finishes again.
    pub(crate) fn dismiss_selected(&mut self) -> Result<(), String> {
        let entry = self.selected_entry().ok_or("nothing selected")?;
        let workspace = self
            .workspace_node(entry)
            .ok_or("only a workspace can be dismissed")?;
        let session = workspace.session.clone();
        let panes: Vec<(String, &'static str)> = workspace
            .tabs
            .iter()
            .flat_map(|tab| &tab.panes)
            .filter_map(|pane| Some((pane.id.clone(), pane.agent.as_ref()?.state.label())))
            .collect();
        let now = now_unix_secs();
        let dismiss = |activity: &mut ActivityState| {
            for (pane_id, status) in &panes {
                activity.dismiss(session.as_deref(), pane_id, status, now);
            }
        };
        update_activity(dismiss)?;
        dismiss(&mut self.activity);
        self.rebuild_glance();
        self.apply_filter();
        Ok(())
    }

    pub(crate) fn directory_template_for_selected(&self) -> Option<&str> {
        let template = self
            .config
            .picker
            .directory_template
            .as_deref()
            .map(str::trim)
            .filter(|template| !template.is_empty())?;
        matches!(
            &self.selected_entry()?.action,
            EntryAction::FocusOrCreateDir
        )
        .then_some(template)
    }

    fn focus_action(&self, action: &EntryAction) -> Result<(), String> {
        #[cfg(test)]
        if let Some(control) = &self.herdr_control {
            return control.focus(action);
        }
        match action {
            EntryAction::FocusPane { id, .. } => crate::herdr::focus_pane(id),
            EntryAction::FocusWorkspace { id, .. } => run_herdr(["workspace", "focus", id]),
            EntryAction::FocusTab { id, .. } => run_herdr(["tab", "focus", id]),
            _ => Err("not a focus action".into()),
        }
    }

    /// Opens the selected entry. A Workspace that needs you opens at the pane
    /// of the agent waiting on the User.
    pub(crate) fn open_selected(&mut self, use_directory_template: bool) -> Result<(), String> {
        let mut e = self.selected_entry().cloned().ok_or("nothing selected")?;
        if let (EntryAction::FocusWorkspace { session, .. }, Some((_, pane))) =
            (&e.action, self.needs_you_pane(&e))
        {
            e.action = EntryAction::FocusPane {
                session: session.clone(),
                id: pane,
            };
        }
        self.open_entry(e, use_directory_template)
    }

    /// The tab and pane of the agent waiting on the User in a glance entry's Workspace.
    pub(crate) fn needs_you_pane(&self, entry: &Entry) -> Option<(String, String)> {
        self.glance_row(entry)?.needs_you_pane.clone()
    }

    /// Opens tab `number` (1-based) of the selected Workspace, or in the
    /// Sessions view the current session's Workspace `number`.
    pub(crate) fn open_tab(&mut self, number: usize) -> Result<(), String> {
        if self.sessions_view() {
            let target = match self.session_action(Some(number)) {
                Some(EntryAction::FocusWorkspace { session, id }) => {
                    workspace_key(session.as_deref(), &id)
                }
                _ => return Err("no such workspace in this session".into()),
            };
            let entry = self
                .topology_entries
                .iter()
                .find(|entry| entry_identity(entry) == target)
                .cloned()
                .ok_or("no such workspace in this session")?;
            return self.open_entry(entry, false);
        }
        let action = self.selected_tab_action(number).ok_or("no such tab")?;
        let mut entry = self.selected_entry().cloned().ok_or("nothing selected")?;
        entry.action = action;
        self.open_entry(entry, false)
    }

    fn open_entry(&mut self, e: Entry, use_directory_template: bool) -> Result<(), String> {
        let action_destination = recent_destination_for_entry(&e);
        let (result, notify_success, notify_failure) = match &e.action {
            EntryAction::FocusPane { .. }
            | EntryAction::FocusWorkspace { .. }
            | EntryAction::FocusTab { .. } => (self.focus_action(&e.action), true, true),
            EntryAction::SwitchSession {
                session,
                workspace,
                tab,
            } => {
                let _ =
                    self.switch_or_notice(session.as_deref(), workspace.clone(), tab.clone(), None);
                (Ok(()), false, false)
            }
            EntryAction::InvokePluginAction { action } => (
                run_herdr(["plugin", "action", "invoke", action]),
                true,
                true,
            ),
            EntryAction::FocusOrCreateDir => (
                self.focus_or_create_dir(&e.path, &e.title, use_directory_template),
                true,
                true,
            ),
            EntryAction::RunCommand {
                command,
                notify_success,
                notify_error,
            } => (
                command::run_command(command),
                *notify_success,
                *notify_error,
            ),
        };

        match result {
            Ok(()) => {
                let destination = action_destination.or_else(|| {
                    recent_destination_after_indirect_success(
                        &e.action,
                        current_workspace_id().ok().as_deref(),
                    )
                });
                if let Some((session, workspace_id)) = destination {
                    self.record_recent_workspace(session.as_deref(), &workspace_id);
                }
                if notify_success {
                    notify_done(&format!("Opened {}", e.title), &self.config.notifications);
                }
                Ok(())
            }
            Err(err) => {
                if notify_failure {
                    notify_error(
                        &format!("Failed {}: {}", e.title, err.trim()),
                        &self.config.notifications,
                    );
                }
                Err(err)
            }
        }
    }

    pub(crate) fn close_selected_workspace(&mut self) -> Result<(), String> {
        let (id, title) = {
            let e = self.selected_entry().ok_or("nothing selected")?;
            if self.is_elsewhere(e) {
                return Err("the workspace is in another Herdr session".into());
            }
            let id = self
                .workspace_to_close(e)
                .ok_or("no open workspace for selected item")?;
            (id, e.title.clone())
        };
        let launcher = launch_workspace_id();
        let destination = if launcher.as_deref() == Some(&id) {
            let previous = self
                .previous_workspace_id
                .as_deref()
                .filter(|previous| *previous != id)
                .ok_or("no previous workspace to focus before closing this one")?;
            run_herdr(["workspace", "focus", previous])?;
            Some(previous.to_string())
        } else if let Some(launcher) = launcher {
            run_herdr(["workspace", "focus", &launcher])?;
            Some(launcher)
        } else {
            None
        };
        run_herdr(["workspace", "close", &id])?;
        if let Some(destination) = destination {
            let session = self.session.clone();
            self.record_recent_workspace(session.as_deref(), &destination);
        }
        notify_done(&format!("Closed {title}"), &self.config.notifications);
        self.refresh();
        Ok(())
    }

    fn record_recent_workspace(&mut self, session: Option<&str>, workspace_id: &str) {
        self.recent_state.record(session, workspace_id);
        self.recent_state.save();
    }

    fn workspace_to_close(&self, e: &Entry) -> Option<String> {
        match e.source {
            Source::Workspace => e.workspace_id.clone(),
            Source::Zoxide | Source::Root => self.matching_dir_workspace(e).map(|ws| ws.id.clone()),
            Source::Session | Source::QuickAction | Source::Integration => None,
        }
    }

    pub(crate) fn focus_or_create_dir(
        &self,
        path: &Path,
        label: &str,
        use_directory_template: bool,
    ) -> Result<(), String> {
        let key = canonical_str(path).unwrap_or_else(|| path.display().to_string());
        if use_directory_template {
            let template_name = self
                .config
                .picker
                .directory_template
                .as_deref()
                .ok_or("no directory_template configured")?;
            let template = herdr_plus::load_project_template(template_name)?;
            if let Some(workspace_id) = self
                .matching_template_workspace_by_key(&key)
                .map(|workspace| workspace.id.clone())
            {
                run_herdr(["workspace", "focus", &workspace_id])?;
                return herdr_plus::append_project_tabs(&template, &workspace_id, path);
            }
            let json = herdr_json([
                "workspace",
                "create",
                "--cwd",
                &path.display().to_string(),
                "--label",
                label,
                "--focus",
            ])?;
            return herdr_plus::bootstrap_project_tabs(&template, &json, path);
        }

        if self.config.picker.reuse_existing {
            if let Some(ws) = self.matching_dir_workspace_by_key(&key) {
                return run_herdr(["workspace", "focus", &ws.id]);
            }
        }
        if !self.config.picker.create_missing {
            return Err("create_missing=false and no workspace exists".into());
        }
        run_herdr([
            "workspace",
            "create",
            "--cwd",
            &path.display().to_string(),
            "--label",
            label,
            "--focus",
        ])
    }

    pub(crate) fn matching_dir_workspace(&self, e: &Entry) -> Option<&WorkspaceRef> {
        self.matching_dir_workspace_by_key(&e.key())
    }

    fn matching_dir_workspace_by_key(&self, key: &str) -> Option<&WorkspaceRef> {
        let workspaces = self.path_to_workspaces.get(key)?;
        workspaces
            .iter()
            .find(|ws| ws.kind == WorkspaceKind::Dir)
            .or_else(|| {
                workspaces
                    .iter()
                    .find(|ws| ws.kind == WorkspaceKind::Unknown)
            })
    }

    fn matching_template_workspace_by_key(&self, key: &str) -> Option<&WorkspaceRef> {
        let workspaces = self.path_to_workspaces.get(key)?;
        workspaces
            .iter()
            .find(|workspace| workspace.kind == WorkspaceKind::Dir)
            .or_else(|| {
                workspaces
                    .iter()
                    .find(|workspace| workspace.kind == WorkspaceKind::Unknown)
            })
    }
}

fn normalize_recent_state(state: &mut RecentState, entries: &[Entry]) {
    let session = entries.iter().find_map(|entry| match &entry.action {
        EntryAction::FocusWorkspace { session, .. } => Some(session.as_deref()),
        _ => None,
    });
    let Some(session) = session else { return };
    let live_ids = entries
        .iter()
        .filter_map(|entry| match &entry.action {
            EntryAction::FocusWorkspace { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let current_id = entries
        .iter()
        .find(|entry| is_focused_workspace_entry(entry))
        .and_then(|entry| entry.workspace_id.as_deref());
    state.normalize(session, &live_ids, current_id);
}

fn session_key(name: Option<&str>) -> String {
    name.unwrap_or("<default>").to_string()
}

fn recent_destination_for_entry(e: &Entry) -> Option<(Option<String>, String)> {
    match &e.action {
        EntryAction::FocusWorkspace { session, id, .. } => Some((session.clone(), id.clone())),
        EntryAction::FocusTab { session, .. } => e
            .workspace_id
            .as_ref()
            .map(|workspace_id| (session.clone(), workspace_id.clone())),
        EntryAction::FocusPane { session, .. } => e
            .workspace_id
            .as_ref()
            .map(|workspace_id| (session.clone(), workspace_id.clone())),
        _ => None,
    }
}

fn recent_destination_after_indirect_success(
    action: &EntryAction,
    resulting_workspace_id: Option<&str>,
) -> Option<(Option<String>, String)> {
    matches!(action, EntryAction::FocusOrCreateDir)
        .then(|| {
            resulting_workspace_id.map(|workspace_id| (sessions::current(), workspace_id.into()))
        })
        .flatten()
}

fn record_persisted_recent_workspace(session: Option<&str>, workspace_id: &str) {
    let mut state = RecentState::load();
    state.record(session, workspace_id);
    state.save();
}

fn is_focused_workspace_entry(entry: &Entry) -> bool {
    entry.search_terms.iter().any(|term| term == "focused")
}

struct Query {
    plain: String,
    agent: Vec<String>,
    agent_only: bool,
    workspace_or_status: Vec<String>,
    path: Vec<String>,
    status: Vec<String>,
    all_agents: bool,
    all_sessions: bool,
    session: Vec<String>,
}

impl Query {
    fn parse(input: &str) -> Self {
        let mut query = Self {
            plain: String::new(),
            agent: vec![],
            agent_only: false,
            workspace_or_status: vec![],
            path: vec![],
            status: vec![],
            all_agents: false,
            all_sessions: false,
            session: vec![],
        };
        let mut plain = Vec::new();
        for raw in input.split_whitespace() {
            let token = raw.to_lowercase();
            if token == "agent" {
                query.agent_only = true;
            } else if let Some(rest) = token.strip_prefix('!') {
                push_token(&mut query.agent, rest);
            } else if let Some(rest) = token.strip_prefix('@') {
                if rest.is_empty() {
                    query.all_agents = true;
                } else {
                    push_token(&mut query.workspace_or_status, rest);
                }
            } else if let Some(rest) = token.strip_prefix('/') {
                push_token(&mut query.path, rest);
            } else if let Some(rest) = token.strip_prefix('#') {
                push_token(&mut query.status, rest);
            } else if let Some(rest) = token.strip_prefix('*') {
                query.all_sessions = true;
                push_token(&mut query.session, rest);
            } else {
                plain.push(token);
            }
        }
        query.plain = plain.join(" ");
        query
    }

    fn filters_match(&self, entry: &Entry) -> bool {
        let agent_query = self.all_agents
            || self.agent_only
            || !self.agent.is_empty()
            || !self.workspace_or_status.is_empty()
            || !self.status.is_empty();
        if agent_query && !is_agent_entry(entry) {
            return false;
        }
        if self.all_sessions {
            let EntryAction::FocusWorkspace { session, .. } = &entry.action else {
                return false;
            };
            if !all_match(&self.session, Session::label(session.as_deref())) {
                return false;
            }
        }
        (!self.agent_only && self.agent.is_empty() || is_agent_entry(entry))
            && all_match(&self.agent, &agent_text(entry))
            && all_match_either(
                &self.workspace_or_status,
                &workspace_text(entry),
                &status_text(entry),
            )
            && all_match(&self.path, &entry.path.display().to_string())
            && all_match(&self.status, &status_text(entry))
    }
}

/// The share of the best match score, in percent, that a row needs to stay in
/// a glance search. Loose fuzzy hits score well below a match on word starts.
const NEAR_BEST_PERCENT: i64 = 85;

fn near_best(score: i64, best: i64) -> bool {
    score * 100 >= best * NEAR_BEST_PERCENT
}

fn push_token(tokens: &mut Vec<String>, value: &str) {
    if !value.is_empty() {
        tokens.push(value.into());
    }
}

fn all_match(tokens: &[String], haystack: &str) -> bool {
    let haystack = haystack.to_lowercase();
    tokens.iter().all(|token| haystack.contains(token))
}

fn all_match_either(tokens: &[String], left: &str, right: &str) -> bool {
    let left = left.to_lowercase();
    let right = right.to_lowercase();
    tokens
        .iter()
        .all(|token| left.contains(token) || right.contains(token))
}

fn is_agent_entry(entry: &Entry) -> bool {
    entry.agent_target.is_some()
        || entry
            .search_terms
            .iter()
            .any(|term| term.starts_with("agent-name:"))
}

fn ordinary_source_label(entry: &Entry) -> &'static str {
    match entry.action {
        EntryAction::FocusWorkspace { .. } => "workspace",
        EntryAction::FocusTab { .. } => "tab",
        EntryAction::FocusPane { .. } => "pane",
        _ => entry.source.label(),
    }
}

fn status_text(entry: &Entry) -> String {
    let statuses = entry
        .search_terms
        .iter()
        .filter_map(|term| term.strip_prefix("agent-status:"))
        .collect::<Vec<_>>();
    if statuses.is_empty() {
        entry
            .subtitle
            .split('·')
            .next()
            .unwrap_or(&entry.subtitle)
            .trim()
            .to_lowercase()
    } else {
        statuses.join(" ")
    }
}

fn agent_text(entry: &Entry) -> String {
    let names = entry
        .search_terms
        .iter()
        .filter_map(|term| term.strip_prefix("agent-name:"))
        .collect::<Vec<_>>();
    if names.is_empty() {
        entry
            .title
            .split('·')
            .next()
            .unwrap_or(&entry.title)
            .to_string()
    } else {
        names.join(" ")
    }
}

fn workspace_text(entry: &Entry) -> String {
    format!(
        "{} {} {}",
        entry.workspace_id.as_deref().unwrap_or(""),
        entry.workspace_label.as_deref().unwrap_or(""),
        entry.title
    )
}

const JUMP_BACK_STATE_FILE: &str = "jump-back-workspace";
const FOCUSED_WORKSPACE_STATE_FILE: &str = "focused-workspace";
const PINNED_ENTRIES_STATE_FILE: &str = "pinned-entries.json";
const COLD_ENTRIES_STATE_FILE: &str = "cold-entries.json";

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct JumpBackState {
    session: String,
    workspace_id: String,
}

fn current_session_identity() -> String {
    env::var("HERDR_SESSION")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "default".into())
}

pub(crate) fn jump_back(config: &Config) -> Result<String, String> {
    if !config.jump_back.enabled {
        return Err("jump back is disabled in config".into());
    }
    let json = herdr_json(["workspace", "list"])?;
    let current = focused_workspace_id(&json)
        .ok_or("can't determine the current workspace")?
        .to_string();
    let previous = read_previous_workspace()?;
    if previous == current {
        return Err("no previous workspace yet".into());
    }
    let Some(label) = workspace_label(&json, &previous).map(str::to_string) else {
        let _ = fs::remove_file(plugin_config_dir().join(JUMP_BACK_STATE_FILE));
        return Err("previous workspace no longer exists".into());
    };

    run_herdr(["workspace", "focus", &previous])?;
    record_persisted_recent_workspace(sessions::current().as_deref(), &previous);
    Ok(label)
}

fn focused_workspace_id(json: &serde_json::Value) -> Option<&str> {
    json.pointer("/result/workspaces")
        .and_then(|v| v.as_array())?
        .iter()
        .find(|workspace| workspace.get("focused").and_then(|v| v.as_bool()) == Some(true))?
        .get("workspace_id")?
        .as_str()
}

fn workspace_label<'a>(json: &'a serde_json::Value, id: &str) -> Option<&'a str> {
    json.pointer("/result/workspaces")
        .and_then(|v| v.as_array())?
        .iter()
        .find(|workspace| workspace.get("workspace_id").and_then(|v| v.as_str()) == Some(id))?
        .get("label")?
        .as_str()
}

fn current_workspace_id() -> Result<String, String> {
    let json = herdr_json(["workspace", "list"])?;
    focused_workspace_id(&json)
        .map(str::to_string)
        .ok_or_else(|| "can't determine the current workspace".into())
}

pub(crate) fn track_focus(config: &Config) -> Result<(), String> {
    let event = env::var("HERDR_PLUGIN_EVENT_JSON")
        .map_err(|_| "HERDR_PLUGIN_EVENT_JSON is not set".to_string())?;
    let event = serde_json::from_str::<serde_json::Value>(&event)
        .map_err(|e| format!("failed to parse focus event: {e}"))?;
    let focused = event
        .pointer("/data/workspace_id")
        .and_then(|v| v.as_str())
        .ok_or("focus event has no workspace_id")?;
    if config.jump_back.enabled {
        record_focused_workspace(focused)?;
    }
    let session = sessions::current();
    let now = now_unix_secs();
    update_activity(|activity| {
        activity.record_focus(session.as_deref(), focused, now);
        activity.record_seen(session.as_deref(), now);
    })
}

/// Records that the User is in the current session.
pub(crate) fn record_seen() -> Result<(), String> {
    let session = sessions::current();
    update_activity(|activity| activity.record_seen(session.as_deref(), now_unix_secs()))
}

pub(crate) fn track_agent() -> Result<(), String> {
    let event = env::var("HERDR_PLUGIN_EVENT_JSON")
        .map_err(|_| "HERDR_PLUGIN_EVENT_JSON is not set".to_string())?;
    let (pane_id, status) = parse_agent_status_event(&event)?;
    let now = now_unix_secs();
    let session = sessions::current();
    update_activity(|activity| {
        activity.record_agent_status(session.as_deref(), &pane_id, &status, now)
    })
}

#[derive(Default)]
pub(crate) struct Workspaces {
    pub(crate) topology: OpenTopology,
    pub(crate) activity: ActivityState,
    pub(crate) previous: Option<String>,
    pub(crate) subagents: HashMap<String, Vec<Subagent>>,
    /// The Herdr session the topology was read from; other sessions' Workspaces join it.
    pub(crate) session: Option<String>,
    /// Every discovered Herdr session, for the Sessions view.
    pub(crate) sessions: Vec<SessionState>,
}

impl Workspaces {
    fn local_topology(&self) -> OpenTopology {
        OpenTopology {
            workspaces: local_workspaces(&self.topology, &self.session)
                .cloned()
                .collect(),
        }
    }
}

/// The Workspaces of `topology` that belong to `session`; other sessions'
/// Workspaces join a Topology only where they need you.
fn local_workspaces<'a>(
    topology: &'a OpenTopology,
    session: &'a Option<String>,
) -> impl Iterator<Item = &'a WorkspaceNode> {
    topology
        .workspaces
        .iter()
        .filter(move |workspace| &workspace.session == session)
}

/// Reads the Herdr snapshot and activity. A `snapshot` already read from Herdr
/// is parsed instead of asking again. Other running sessions add the
/// Workspaces that need you. Git identities come from `cached` when given,
/// and from git itself otherwise.
pub(crate) fn load_workspaces(
    config: &Config,
    cached: Option<&OpenTopology>,
    snapshot: Option<&serde_json::Value>,
) -> Workspaces {
    let session = sessions::current();
    let activity = ActivityState::load();
    let mut states = Vec::new();
    let topology = if config.sources.open_workspaces {
        let mut topology = snapshot
            .map(|snapshot| topology::parse_snapshot(snapshot, session.clone()))
            .filter(|topology| !topology.workspaces.is_empty())
            .unwrap_or_else(|| {
                topology::collect_topology(
                    |args| herdr_json_args(args.iter().copied()),
                    session.clone(),
                )
            });
        states = sessions::read_states();
        join_needs_you(&mut topology, sessions::other_topology(&states), &activity);
        match cached {
            Some(cached) => reuse_git(&mut topology, cached),
            None => topology::enrich_git(&mut topology, &topology::NativeGitProbe),
        }
        topology
    } else {
        OpenTopology::default()
    };
    let now = now_unix_secs();
    let subagents = detail::scan_subagents(&topology, &paths::home().join(".claude/projects"), now);
    Workspaces {
        topology,
        activity,
        previous: config
            .jump_back
            .enabled
            .then(|| read_previous_workspace().ok())
            .flatten(),
        subagents,
        session,
        sessions: states,
    }
}

/// Adds the Workspaces of other sessions that need you. The glance shows no
/// other rows of theirs, so the rest skip git and transcript reads.
fn join_needs_you(topology: &mut OpenTopology, others: OpenTopology, activity: &ActivityState) {
    topology
        .workspaces
        .extend(others.workspaces.into_iter().filter(|workspace| {
            glance::needs_you(&glance::evidence(workspace, activity, false, None, true))
        }));
}

/// Copies git identities from `cached`. Workspaces the cache does not know
/// are enriched right away.
fn reuse_git(topology: &mut OpenTopology, cached: &OpenTopology) {
    let mut unknown = Vec::new();
    for (index, workspace) in topology.workspaces.iter_mut().enumerate() {
        match cached
            .workspaces
            .iter()
            .find(|known| known.id == workspace.id && known.session == workspace.session)
        {
            Some(known) => workspace.git = known.git.clone(),
            None => unknown.push(index),
        }
    }
    if unknown.is_empty() {
        return;
    }
    let mut probe = OpenTopology {
        workspaces: unknown
            .iter()
            .map(|index| topology.workspaces[*index].clone())
            .collect(),
    };
    topology::enrich_git(&mut probe, &topology::NativeGitProbe);
    for (index, enriched) in unknown.into_iter().zip(probe.workspaces) {
        topology.workspaces[index].git = enriched.git;
    }
}

const DETAIL_TTL_SECS: i64 = 10;
const SESSION_GIT_TTL_SECS: i64 = 30;

/// A Sessions view row's facts about its session.
pub(crate) struct SessionSummary {
    /// The most urgent of blocked, done and working across its Workspaces.
    pub(crate) state: Option<AgentState>,
    /// The most urgent Needs you state and how many Workspaces need you.
    pub(crate) needs_you: Option<(AgentState, usize)>,
    pub(crate) workspaces: usize,
    /// The newest Workspace focus or agent status change.
    pub(crate) last_active: Option<i64>,
    pub(crate) last_focused: Option<String>,
}

pub(crate) struct SelectedWorkspace<'a> {
    pub(crate) entry: &'a Entry,
    pub(crate) workspace: &'a topology::WorkspaceNode,
    pub(crate) row: &'a GlanceRow,
    pub(crate) key: String,
}

fn entry_identity(entry: &Entry) -> String {
    match &entry.action {
        EntryAction::FocusWorkspace { session, id } => workspace_key(session.as_deref(), id),
        _ => entry.key(),
    }
}

/// Prunes the stored activity against the live snapshot and returns it. An
/// empty snapshot is a failed read, so it prunes nothing.
fn prune_activity(topology: &OpenTopology) -> Option<ActivityState> {
    if topology.workspaces.is_empty() {
        return None;
    }
    ActivityState::update(|activity| activity.retain_topology(topology)).ok()
}

fn update_activity(update: impl FnOnce(&mut ActivityState)) -> Result<(), String> {
    ActivityState::update(update)
        .map(|_| ())
        .map_err(|e| format!("failed to save activity state: {e}"))
}

fn record_focused_workspace(focused: &str) -> Result<(), String> {
    if let Ok(last) = read_workspace_state(FOCUSED_WORKSPACE_STATE_FILE) {
        if last != focused {
            save_previous_workspace(&last)?;
        }
    }
    save_workspace_state(FOCUSED_WORKSPACE_STATE_FILE, focused)
}

fn decode_previous_workspace(
    value: &str,
    current_session: &str,
    allow_legacy_default: bool,
) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("no previous workspace yet".into());
    }
    if let Ok(state) = serde_json::from_str::<JumpBackState>(value) {
        return if state.session == current_session {
            Ok(state.workspace_id)
        } else {
            Err("previous workspace belongs to another session".into())
        };
    }
    if allow_legacy_default && current_session == "default" {
        Ok(value.into())
    } else {
        Err("legacy jump-back state has no safe session identity".into())
    }
}

fn encode_previous_workspace(session: &str, workspace_id: &str) -> Result<String, String> {
    serde_json::to_string(&JumpBackState {
        session: session.into(),
        workspace_id: workspace_id.into(),
    })
    .map_err(|error| format!("failed to encode jump-back state: {error}"))
}

fn read_previous_workspace() -> Result<String, String> {
    read_workspace_state(JUMP_BACK_STATE_FILE)
}

fn read_workspace_state(file: &str) -> Result<String, String> {
    let path = plugin_config_dir().join(file);
    let value = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "no previous workspace yet".into()
        } else {
            format!("failed to read jump-back state: {error}")
        }
    })?;
    let named_session = env::var("HERDR_SESSION")
        .ok()
        .is_some_and(|name| !name.trim().is_empty());
    decode_previous_workspace(&value, &current_session_identity(), !named_session)
}

fn save_previous_workspace(id: &str) -> Result<(), String> {
    save_workspace_state(JUMP_BACK_STATE_FILE, id)
}

fn save_workspace_state(file: &str, id: &str) -> Result<(), String> {
    let dir = plugin_config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create plugin config: {e}"))?;
    let value = encode_previous_workspace(&current_session_identity(), id)?;
    fs::write(dir.join(file), value).map_err(|e| format!("failed to save workspace state: {e}"))
}

fn read_entry_keys(path: &Path) -> Result<HashSet<String>, String> {
    let value =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str::<Vec<String>>(&value)
        .map(|entries| entries.into_iter().collect())
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

fn save_entry_keys(path: &Path, entries: &HashSet<String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create plugin config: {e}"))?;
    }
    let mut entries = entries.iter().collect::<Vec<_>>();
    entries.sort_unstable();
    let value = serde_json::to_string_pretty(&entries)
        .map_err(|e| format!("failed to encode {}: {e}", path.display()))?;
    fs::write(path, value).map_err(|e| format!("failed to save {}: {e}", path.display()))
}

/// Reads a plain JSON array of keys as entries sent at 0.
fn read_cold_entries(path: &Path) -> Result<HashMap<String, i64>, String> {
    let value =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    serde_json::from_str::<HashMap<String, i64>>(&value)
        .or_else(|_| {
            serde_json::from_str::<Vec<String>>(&value)
                .map(|keys| keys.into_iter().map(|key| (key, 0)).collect())
        })
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))
}

fn save_cold_entries(path: &Path, entries: &HashMap<String, i64>) -> Result<(), String> {
    let entries = entries.iter().collect::<BTreeMap<_, _>>();
    let value = serde_json::to_vec_pretty(&entries)
        .map_err(|e| format!("failed to encode {}: {e}", path.display()))?;
    sessions::write_atomic(path, &value)
        .map_err(|e| format!("failed to save {}: {e}", path.display()))
}

fn launch_workspace_id() -> Option<String> {
    env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("workspace_id")?.as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
}

fn topology_workspace_pin_key(session: Option<&str>, id: &str) -> String {
    format!("workspace:{}:{id}", session_key(session))
}

fn legacy_current_workspace_pin_key(_entry: &Entry) -> Option<String> {
    None
}

fn migrate_legacy_topology_pins(entries: &[Entry], pins: &mut HashSet<String>) -> bool {
    let mut changed = false;
    let legacy = pins
        .iter()
        .filter_map(|key| {
            key.strip_prefix("agent:")
                .map(|target| (key.clone(), target.to_string()))
        })
        .collect::<Vec<_>>();
    for (legacy_key, target) in legacy {
        let Some(entry) = entries.iter().find(|entry| {
            entry.agent_target.as_deref() == Some(target.as_str())
                && matches!(entry.action, EntryAction::FocusPane { .. })
        }) else {
            continue;
        };
        pins.remove(&legacy_key);
        pins.insert(pin_key(entry));
        changed = true;
    }
    changed
}

pub(crate) fn pin_key(entry: &Entry) -> String {
    match &entry.action {
        EntryAction::FocusWorkspace { session, id, .. } => {
            topology_workspace_pin_key(session.as_deref(), id)
        }
        EntryAction::FocusTab { session, id, .. } => {
            format!("tab:{}:{id}", session_key(session.as_deref()))
        }
        EntryAction::FocusPane { session, id } => {
            format!("pane:{}:{id}", session_key(session.as_deref()))
        }
        EntryAction::SwitchSession { session, .. } => {
            format!("session:{}", session_key(session.as_deref()))
        }
        EntryAction::InvokePluginAction { action } => {
            format!("plugin:{}:{action}", entry.source_name())
        }
        EntryAction::FocusOrCreateDir => format!("{}:{}", entry.source.label(), entry.key()),
        EntryAction::RunCommand { command, .. } => format!("{}:{command}", entry.source_name()),
    }
}

fn push_unique(entries: &mut Vec<Entry>, seen: &mut HashSet<String>, incoming: Vec<Entry>) {
    for e in incoming {
        let path_key = e.key();
        let key = match &e.action {
            EntryAction::FocusWorkspace { session, id, .. } => {
                format!("open:workspace:{}:{id}", session_key(session.as_deref()))
            }
            EntryAction::FocusTab { session, id, .. } => {
                format!("open:tab:{}:{id}", session_key(session.as_deref()))
            }
            EntryAction::RunCommand { command, .. } => format!("{}:{command}", e.source_name()),
            _ if matches!(e.source, Source::Zoxide | Source::Root) => {
                format!("directory:{path_key}")
            }
            _ => format!("{}:{path_key}", e.source_name()),
        };
        if seen.insert(key) {
            entries.push(e);
        } else if e.source == Source::Root {
            if let Some(index) = entries.iter().position(|entry| {
                matches!(entry.source, Source::Zoxide | Source::Root) && entry.key() == path_key
            }) {
                entries[index] = e;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        glance::Group,
        topology::{AgentMeta, AgentState, GitHead, GitIdentity, PaneNode, TabNode, WorkspaceNode},
    };
    use std::{
        env,
        ffi::OsString,
        fs,
        path::PathBuf,
        sync::MutexGuard,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    const NOW: i64 = 1_000_000;

    use super::*;
    use crate::{
        config::{Config, JumpBackConfig},
        paths::recent_workspaces_state_path,
        theme::Theme,
    };

    fn entry(source: Source, path: &str, title: &str) -> Entry {
        Entry {
            source,
            title: title.into(),
            subtitle: String::new(),
            path: PathBuf::from(path),
            workspace_id: None,
            workspace_label: None,
            agent_target: None,
            action: EntryAction::FocusOrCreateDir,
            source_label: None,
            search_terms: vec![],
        }
    }

    fn workspace(id: &str, label: &str, kind: WorkspaceKind, path: &str) -> WorkspaceRef {
        WorkspaceRef {
            id: id.into(),
            label: label.into(),
            kind,
            path: PathBuf::from(path),
            tab_count: 1,
            pane_count: 1,
        }
    }

    fn open_workspace(session: &str, id: &str, title: &str, _focused: bool) -> Entry {
        Entry {
            source: Source::Workspace,
            title: title.into(),
            subtitle: format!("agent:unknown · {id} tabs:2 panes:2"),
            path: PathBuf::from(format!("/tmp/{title}")),
            workspace_id: Some(id.into()),
            workspace_label: Some(title.into()),
            agent_target: None,
            action: EntryAction::FocusWorkspace {
                session: Some(session.into()),
                id: id.into(),
            },
            source_label: None,
            search_terms: vec![session.into(), id.into(), title.into()],
        }
    }

    fn open_tab(session: &str, workspace_id: &str, id: &str, title: &str, _focused: bool) -> Entry {
        Entry {
            source: Source::Workspace,
            title: title.into(),
            subtitle: format!("{id} · 1 panes"),
            path: PathBuf::from(format!("/tmp/{workspace_id}")),
            workspace_id: Some(workspace_id.into()),
            workspace_label: None,
            agent_target: None,
            action: EntryAction::FocusTab {
                session: Some(session.into()),
                id: id.into(),
            },
            source_label: None,
            search_terms: vec![session.into(), workspace_id.into(), id.into(), title.into()],
        }
    }

    struct CommandTestEnv {
        _lock: MutexGuard<'static, ()>,
        dir: PathBuf,
        previous_vars: Vec<(&'static str, Option<OsString>)>,
    }

    impl Drop for CommandTestEnv {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
            for (name, value) in &self.previous_vars {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
    }

    fn command_test_env() -> CommandTestEnv {
        let lock = crate::herdr::test_env_lock();
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("helm-app-command-test-{suffix}"));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("herdr");
        fs::write(
            &script,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$HERDR_TEST_LOG"
if [ "$1" = "workspace" ] && [ "$2" = "list" ]; then
  printf '%s\n' '{"result":{"workspaces":[{"workspace_id":"w-current","label":"Current","focused":true},{"workspace_id":"w-previous","label":"Previous","focused":false}]}}'
  exit 0
fi
if [ "$HERDR_TEST_MODE" = "fail" ]; then
  exit 1
fi
if [ "$HERDR_TEST_MODE" = "focus-fail" ] && [ "$1" = "workspace" ] && [ "$2" = "focus" ]; then
  exit 1
fi
exit 0
"#,
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let names = [
            "HERDR_BIN_PATH",
            "HERDR_PLUGIN_CONFIG_DIR",
            "HERDR_PLUGIN_CONTEXT_JSON",
            "HERDR_PLUGIN_EVENT_JSON",
            "HERDR_SESSION",
            "HERDR_TEST_LOG",
            "HERDR_TEST_MODE",
        ];
        let previous_vars = names
            .into_iter()
            .map(|name| (name, env::var_os(name)))
            .collect();
        env::set_var("HERDR_BIN_PATH", &script);
        env::set_var("HERDR_PLUGIN_CONFIG_DIR", &dir);
        env::set_var(
            "HERDR_PLUGIN_CONTEXT_JSON",
            r#"{"workspace_id":"w-launch"}"#,
        );
        env::set_var("HERDR_SESSION", "work");
        env::set_var("HERDR_TEST_LOG", dir.join("calls"));
        env::set_var("HERDR_TEST_MODE", "success");

        CommandTestEnv {
            _lock: lock,
            dir,
            previous_vars,
        }
    }

    fn command_test_config() -> Config {
        let mut config = Config::default();
        config.notifications.enabled = false;
        config.sources.open_workspaces = false;
        config.sources.zoxide = false;
        config.sources.roots = false;
        config.sources.herdr_plus_quick_actions = false;
        config
    }

    fn app_with_selected_entry(config: Config, entry: Entry) -> App {
        let mut app = App::new(config, Theme::terminal());
        app.entries = vec![entry];
        app.filtered = vec![0];
        app
    }

    #[test]
    fn exact_agent_token_is_a_trait_predicate_and_agents_is_plain_text() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let mut agent = entry(Source::Integration, "/tmp", "Codex");
        agent.agent_target = Some("p1".into());
        let tab = entry(Source::Integration, "/tmp", "Agents");
        app.entries = vec![agent, tab];

        app.source_filter = Some(Source::Integration);
        app.query = "agent".into();
        app.apply_filter();
        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected_entry().unwrap().title, "Codex");

        app.query = "agents".into();
        app.apply_filter();
        assert_eq!(app.selected_entry().unwrap().title, "Agents");
    }

    #[test]
    fn legacy_agent_marks_migrate_to_session_qualified_pane_keys() {
        let mut pane = entry(Source::Workspace, "/tmp", "Shell");
        pane.agent_target = Some("p1".into());
        pane.action = EntryAction::FocusPane {
            session: Some("dev".into()),
            id: "p1".into(),
        };
        let mut pins = HashSet::from(["agent:p1".to_string()]);

        assert!(migrate_legacy_topology_pins(&[pane], &mut pins));
        assert_eq!(pins, HashSet::from(["pane:dev:p1".to_string()]));
    }

    #[test]
    fn marked_flat_destinations_use_bookmark_source_without_duplicates() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.entries = vec![entry(Source::Root, "/tmp/project", "Project")];
        let key = pin_key(&app.entries[0]);
        app.pinned_entries.insert(key);
        app.source_filter = Some(Source::Root);
        app.apply_filter();

        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected_entry().unwrap().source_name(), "bookmark");
    }

    #[test]
    fn equal_score_ties_preserve_insertion_order() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.entries = vec![
            entry(Source::Zoxide, "/zulu", "zulu"),
            entry(Source::Zoxide, "/alpha", "alpha"),
        ];

        app.source_filter = Some(Source::Zoxide);
        app.apply_filter();

        assert_eq!(app.selected_entry().unwrap().title, "zulu");
    }

    #[test]
    fn pinned_entries_sort_first_and_persist() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.entries = vec![
            entry(Source::Root, "/alpha", "alpha"),
            entry(Source::Root, "/zulu", "zulu"),
        ];
        app.pinned_entries.insert(pin_key(&app.entries[1]));
        app.source_filter = Some(Source::Root);
        app.apply_filter();

        assert_eq!(app.selected_entry().unwrap().title, "zulu");

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!("herdr-pins-{suffix}.json"));
        save_entry_keys(&path, &app.pinned_entries).unwrap();
        assert_eq!(read_entry_keys(&path).unwrap(), app.pinned_entries);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn offers_directory_template_for_new_and_existing_directory_workspaces() {
        let mut config = Config::default();
        config.picker.directory_template = Some("default.toml".into());
        let mut app = App::new(config, Theme::terminal());
        app.entries = vec![entry(Source::Zoxide, "/tmp", "tmp")];
        app.source_filter = Some(Source::Zoxide);
        app.apply_filter();

        assert_eq!(app.directory_template_for_selected(), Some("default.toml"));

        app.path_to_workspaces.insert(
            "/tmp".into(),
            vec![workspace("w2", "dir: tmp", WorkspaceKind::Dir, "/tmp")],
        );
        assert_eq!(app.directory_template_for_selected(), Some("default.toml"));

        app.config.picker.create_missing = false;
        assert_eq!(app.directory_template_for_selected(), Some("default.toml"));

        app.path_to_workspaces.insert(
            "/tmp".into(),
            vec![workspace(
                "w1",
                "project: tmp",
                WorkspaceKind::Project,
                "/tmp",
            )],
        );
        assert!(app.matching_template_workspace_by_key("/tmp").is_none());
    }

    #[test]
    fn close_target_matches_entry_kind() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let dir = entry(Source::Root, "/tmp", "tmp");
        app.path_to_workspaces.insert(
            dir.key(),
            vec![
                workspace("w1", "project: tmp", WorkspaceKind::Project, "/tmp"),
                workspace("w2", "dir: tmp", WorkspaceKind::Dir, "/tmp"),
            ],
        );

        assert_eq!(app.workspace_to_close(&dir), Some("w2".into()));
    }

    #[test]
    fn workspace_rows_are_not_deduped_by_path() {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        push_unique(
            &mut entries,
            &mut seen,
            vec![
                Entry {
                    workspace_id: Some("w1".into()),
                    action: EntryAction::FocusWorkspace {
                        session: None,
                        id: "w1".into(),
                    },
                    ..entry(Source::Workspace, "/tmp", "project: tmp")
                },
                Entry {
                    workspace_id: Some("w2".into()),
                    action: EntryAction::FocusWorkspace {
                        session: None,
                        id: "w2".into(),
                    },
                    ..entry(Source::Workspace, "/tmp", "dir: tmp")
                },
            ],
        );

        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn path_entries_deduplicate_across_sources_and_root_wins() {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        push_unique(
            &mut entries,
            &mut seen,
            vec![
                entry(Source::Root, "/tmp/project", "root"),
                entry(Source::Zoxide, "/tmp/project", "zoxide"),
            ],
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, Source::Root);
        assert_eq!(entries[0].title, "root");
    }

    #[test]
    fn open_selected_failure_leaves_recent_state_and_persistence_unchanged() {
        let _env = command_test_env();
        env::set_var("HERDR_TEST_MODE", "fail");
        let mut initial = RecentState::default();
        initial.record(Some("work"), "existing");
        initial.save();
        let before = fs::read(recent_workspaces_state_path()).unwrap();
        let mut app = app_with_selected_entry(
            command_test_config(),
            open_workspace("work", "w1", "Workspace", false),
        );
        app.recent_state = RecentState::load();
        let state_before = app.recent_state.clone();

        assert!(app.open_selected(false).is_err());

        assert_eq!(app.recent_state, state_before);
        assert_eq!(fs::read(recent_workspaces_state_path()).unwrap(), before);
    }

    #[test]
    fn close_selected_workspace_records_and_persists_its_successful_destination() {
        let _env = command_test_env();
        let mut app = app_with_selected_entry(
            command_test_config(),
            open_workspace("work", "w1", "Workspace", false),
        );
        app.session = Some("work".into());

        app.close_selected_workspace().unwrap();

        assert_eq!(RecentState::load().recent_ids(Some("work")), &["w-launch"]);
        let calls = fs::read_to_string(env::var("HERDR_TEST_LOG").unwrap()).unwrap();
        assert!(calls.lines().any(|line| line == "workspace focus w-launch"));
        assert!(calls.lines().any(|line| line == "workspace close w1"));
    }

    #[test]
    fn close_selected_workspace_failure_leaves_recent_state_and_persistence_unchanged() {
        let _env = command_test_env();
        env::set_var("HERDR_TEST_MODE", "fail");
        let mut initial = RecentState::default();
        initial.record(Some("work"), "existing");
        initial.save();
        let before = fs::read(recent_workspaces_state_path()).unwrap();
        let mut app = app_with_selected_entry(
            command_test_config(),
            open_workspace("work", "w1", "Workspace", false),
        );
        app.recent_state = RecentState::load();
        let state_before = app.recent_state.clone();

        assert!(app.close_selected_workspace().is_err());

        assert_eq!(app.recent_state, state_before);
        assert_eq!(fs::read(recent_workspaces_state_path()).unwrap(), before);
        let calls = fs::read_to_string(env::var("HERDR_TEST_LOG").unwrap()).unwrap();
        assert!(!calls.lines().any(|line| line == "workspace close w1"));
    }

    #[test]
    fn jump_back_records_and_persists_only_after_focus_succeeds() {
        let _env = command_test_env();
        let config = Config {
            jump_back: JumpBackConfig { enabled: true },
            ..Config::default()
        };
        save_previous_workspace("w-previous").unwrap();

        assert_eq!(jump_back(&config).unwrap(), "Previous");

        assert_eq!(
            RecentState::load().recent_ids(Some("work")),
            &["w-previous"]
        );
    }

    #[test]
    fn jump_back_focus_failure_leaves_recent_state_and_persistence_unchanged() {
        let _env = command_test_env();
        env::set_var("HERDR_TEST_MODE", "focus-fail");
        let mut initial = RecentState::default();
        initial.record(Some("work"), "existing");
        initial.save();
        save_previous_workspace("w-previous").unwrap();
        let recent_before = fs::read(recent_workspaces_state_path()).unwrap();
        let previous_before = fs::read(plugin_config_dir().join(JUMP_BACK_STATE_FILE)).unwrap();
        let config = Config {
            jump_back: JumpBackConfig { enabled: true },
            ..Config::default()
        };

        assert!(jump_back(&config).is_err());

        assert_eq!(
            fs::read(recent_workspaces_state_path()).unwrap(),
            recent_before
        );
        assert_eq!(
            fs::read(plugin_config_dir().join(JUMP_BACK_STATE_FILE)).unwrap(),
            previous_before
        );
    }

    #[test]
    fn successful_focus_actions_resolve_workspace_destinations() {
        let workspace = open_workspace("work", "w1", "Workspace", false);
        assert_eq!(
            recent_destination_for_entry(&workspace),
            Some((Some("work".into()), "w1".into()))
        );

        let tab = open_tab("work", "w2", "w2:t1", "Tab", false);
        assert_eq!(
            recent_destination_for_entry(&tab),
            Some((Some("work".into()), "w2".into()))
        );

        let mut pane = entry(Source::Workspace, "/tmp/pane", "Shell");
        pane.workspace_id = Some("w4".into());
        pane.action = EntryAction::FocusPane {
            session: Some("work".into()),
            id: "w4:p1".into(),
        };
        assert_eq!(
            recent_destination_for_entry(&pane),
            Some((Some("work".into()), "w4".into()))
        );
    }

    #[test]
    fn indirect_success_uses_resulting_workspace_and_failure_has_no_destination() {
        let action = EntryAction::FocusOrCreateDir;
        let destination = recent_destination_after_indirect_success(&action, Some("w4"));
        assert_eq!(
            destination
                .as_ref()
                .map(|(_, workspace_id)| workspace_id.as_str()),
            Some("w4")
        );

        let destination_session = destination
            .as_ref()
            .and_then(|(session, _)| session.clone());
        let mut state = RecentState::default();
        if let Some((session, workspace_id)) = destination {
            state.record(session.as_deref(), &workspace_id);
        }
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.recent_ids(destination_session.as_deref()), &["w4"]);
        let unchanged = state.clone();
        assert!(recent_destination_after_indirect_success(&action, None).is_none());
        assert_eq!(state, unchanged);
    }

    #[test]
    fn focus_changes_record_the_workspace_left_behind() {
        let _env = command_test_env();

        record_focused_workspace("w1").unwrap();
        assert!(read_previous_workspace().is_err());

        record_focused_workspace("w2").unwrap();
        assert_eq!(read_previous_workspace().unwrap(), "w1");

        record_focused_workspace("w2").unwrap();
        assert_eq!(read_previous_workspace().unwrap(), "w1");

        record_focused_workspace("w1").unwrap();
        assert_eq!(read_previous_workspace().unwrap(), "w2");
    }

    #[test]
    fn jump_back_state_round_trips_only_in_the_same_session() {
        let encoded = encode_previous_workspace("work", "w1").unwrap();
        assert_eq!(
            decode_previous_workspace(&encoded, "work", false).unwrap(),
            "w1"
        );
        assert_eq!(
            serde_json::from_str::<JumpBackState>(&encoded).unwrap(),
            JumpBackState {
                session: "work".into(),
                workspace_id: "w1".into()
            }
        );
    }

    #[test]
    fn jump_back_state_rejects_cross_session_id_collisions() {
        let encoded = encode_previous_workspace("work", "same-id").unwrap();
        assert!(decode_previous_workspace(&encoded, "personal", false).is_err());
        assert!(decode_previous_workspace("same-id", "personal", false).is_err());
        assert_eq!(
            decode_previous_workspace("legacy-w1", "default", true).unwrap(),
            "legacy-w1"
        );
    }

    #[test]
    fn jump_back_resolves_focused_and_previous_workspaces() {
        let json = serde_json::json!({
            "result": {"workspaces": [
                {"workspace_id": "w1", "label": "one", "focused": false},
                {"workspace_id": "w2", "label": "two", "focused": true}
            ]}
        });

        assert_eq!(focused_workspace_id(&json), Some("w2"));
        assert_eq!(workspace_label(&json, "w1"), Some("one"));
        assert_eq!(workspace_label(&json, "missing"), None);
    }

    fn bare_workspace(
        id: &str,
        label: &str,
        focused: bool,
        commit_at: Option<i64>,
    ) -> WorkspaceNode {
        WorkspaceNode {
            id: id.into(),
            label: label.into(),
            session: None,
            focused,
            tabs: vec![],
            git: commit_at.map(|at| GitIdentity {
                repo_key: label.into(),
                label: label.into(),
                head: GitHead::Branch("main".into()),
                last_commit_at: Some(at),
                status: None,
            }),
        }
    }

    fn glance_titles(app: &App) -> Vec<String> {
        app.filtered
            .iter()
            .map(|index| app.entries[*index].title.clone())
            .collect()
    }

    #[test]
    fn opening_selects_the_focused_workspace() {
        let dir = env::temp_dir().join(format!("helm-initial-test-{}", std::process::id()));
        let _lock = crate::herdr::test_env_lock();
        let previous = env::var_os("HERDR_PLUGIN_CONFIG_DIR");
        env::set_var("HERDR_PLUGIN_CONFIG_DIR", &dir);

        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        bare_workspace("w1", "helm", false, Some(NOW - 60)),
                        bare_workspace("w2", "dots", true, Some(NOW - 120)),
                    ],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );

        assert_eq!(glance_titles(&app), ["helm", "dots"]);
        assert_eq!(app.selected_entry().unwrap().title, "dots");

        let _ = fs::remove_dir_all(&dir);
        match previous {
            Some(value) => env::set_var("HERDR_PLUGIN_CONFIG_DIR", value),
            None => env::remove_var("HERDR_PLUGIN_CONFIG_DIR"),
        }
    }

    #[test]
    fn cold_workspace_drops_to_the_cold_group_and_loses_its_pin() {
        let dir = env::temp_dir().join(format!("helm-cold-test-{}", std::process::id()));
        let _lock = crate::herdr::test_env_lock();
        let previous = env::var_os("HERDR_PLUGIN_CONFIG_DIR");
        env::set_var("HERDR_PLUGIN_CONFIG_DIR", &dir);

        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        bare_workspace("w1", "helm", true, Some(NOW - 60)),
                        bare_workspace("w2", "dots", false, Some(NOW - 120)),
                    ],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        app.selected = app
            .filtered
            .iter()
            .position(|index| app.entries[*index].title == "helm")
            .unwrap();
        app.toggle_selected_pin().unwrap();
        app.toggle_selected_cold().unwrap();

        assert_eq!(glance_titles(&app), ["dots", "helm"]);
        let helm = app.entries[app.filtered[1]].clone();
        assert_eq!(app.glance_group(&helm), Some(Group::Cold));
        assert!(!app.is_pinned(&helm));
        assert_eq!(
            read_cold_entries(&dir.join(COLD_ENTRIES_STATE_FILE)).unwrap(),
            app.cold_entries
        );
        assert!(app.cold_entries[&pin_key(&helm)] >= now_unix_secs() - 5);

        app.selected = 1;
        app.toggle_selected_cold().unwrap();
        assert_eq!(app.glance_group(&helm), Some(Group::Hot));

        let _ = fs::remove_dir_all(&dir);
        match previous {
            Some(value) => env::set_var("HERDR_PLUGIN_CONFIG_DIR", value),
            None => env::remove_var("HERDR_PLUGIN_CONFIG_DIR"),
        }
    }

    #[test]
    fn cold_entries_read_the_array_format_as_sent_at_zero_and_warm_with_newer_activity() {
        let dir = env::temp_dir().join(format!("helm-cold-format-test-{}", std::process::id()));
        let _lock = crate::herdr::test_env_lock();
        let previous = env::var_os("HERDR_PLUGIN_CONFIG_DIR");
        env::set_var("HERDR_PLUGIN_CONFIG_DIR", &dir);
        let path = dir.join(COLD_ENTRIES_STATE_FILE);
        fs::create_dir_all(&dir).unwrap();
        let helm_key = topology_workspace_pin_key(None, "w1");
        let dots_key = topology_workspace_pin_key(None, "w2");
        fs::write(&path, serde_json::to_string(&[&helm_key]).unwrap()).unwrap();

        let entries = read_cold_entries(&path).unwrap();
        assert_eq!(entries, HashMap::from([(helm_key.clone(), 0)]));

        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        bare_workspace("w1", "helm", true, Some(NOW - 60)),
                        bare_workspace("w2", "dots", false, Some(NOW - 120)),
                    ],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        app.cold_entries = HashMap::from([(helm_key.clone(), 0), (dots_key.clone(), NOW - 120)]);
        app.rebuild_glance();
        let helm = app.entries[app.filtered[0]].clone();
        let dots = app.entries[app.filtered[1]].clone();
        assert_eq!(helm.title, "helm");
        assert_eq!(app.glance_group(&helm), Some(Group::Hot));
        assert!(!app.is_cold(&helm));
        assert_eq!(app.glance_group(&dots), Some(Group::Cold));
        assert!(app.is_cold(&dots));

        save_cold_entries(&path, &app.cold_entries).unwrap();
        let written: HashMap<String, i64> =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written, app.cold_entries);

        let _ = fs::remove_dir_all(&dir);
        match previous {
            Some(value) => env::set_var("HERDR_PLUGIN_CONFIG_DIR", value),
            None => env::remove_var("HERDR_PLUGIN_CONFIG_DIR"),
        }
    }

    #[test]
    fn pinned_workspace_stays_in_the_pinned_group() {
        let dir = env::temp_dir().join(format!("helm-pin-test-{}", std::process::id()));
        let _lock = crate::herdr::test_env_lock();
        let previous = env::var_os("HERDR_PLUGIN_CONFIG_DIR");
        env::set_var("HERDR_PLUGIN_CONFIG_DIR", &dir);

        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        bare_workspace("w1", "helm", true, Some(NOW - 60)),
                        bare_workspace("w2", "dots", false, None),
                    ],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        app.selected = app
            .filtered
            .iter()
            .position(|index| app.entries[*index].title == "dots")
            .unwrap();
        app.toggle_selected_pin().unwrap();

        assert_eq!(glance_titles(&app), ["dots", "helm"]);
        assert_eq!(
            app.glance_group(&app.entries[app.filtered[0]]),
            Some(Group::Pinned)
        );
        assert_eq!(
            app.glance_group(&app.entries[app.filtered[1]]),
            Some(Group::Hot)
        );

        let _ = fs::remove_dir_all(&dir);
        match previous {
            Some(value) => env::set_var("HERDR_PLUGIN_CONFIG_DIR", value),
            None => env::remove_var("HERDR_PLUGIN_CONFIG_DIR"),
        }
    }

    #[test]
    fn each_group_lists_the_most_recent_activity_first() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        bare_workspace("w1", "older", false, Some(NOW - 2 * 3_600)),
                        bare_workspace("w2", "newer", false, Some(NOW - 3_600)),
                    ],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        assert_eq!(glance_titles(&app), ["newer", "older"]);
    }

    #[test]
    fn reloading_workspaces_keeps_the_selection_on_the_same_workspace() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let snapshot = |workspaces| Workspaces {
            topology: OpenTopology { workspaces },
            activity: ActivityState::default(),
            previous: None,
            subagents: HashMap::new(),
            session: None,
            sessions: Vec::new(),
        };
        app.apply_workspaces(
            snapshot(vec![
                bare_workspace("w1", "alpha", false, Some(NOW - 2 * 3_600)),
                bare_workspace("w2", "beta", false, Some(NOW - 3_600)),
            ]),
            NOW,
        );
        app.selected = app
            .filtered
            .iter()
            .position(|index| app.entries[*index].title == "alpha")
            .unwrap();

        app.apply_workspaces(
            snapshot(vec![
                bare_workspace("w1", "alpha", false, Some(NOW - 2 * 3_600)),
                bare_workspace("w2", "beta", false, Some(NOW - 3_600)),
                bare_workspace("w3", "gamma", true, Some(NOW - 300)),
            ]),
            NOW,
        );

        assert_eq!(glance_titles(&app), ["gamma", "beta", "alpha"]);
        assert_eq!(app.selected_entry().unwrap().title, "alpha");
    }

    #[test]
    fn empty_reload_keeps_the_last_good_workspaces_and_selection() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let snapshot = |workspaces| Workspaces {
            topology: OpenTopology { workspaces },
            activity: ActivityState::default(),
            previous: None,
            subagents: HashMap::new(),
            session: None,
            sessions: Vec::new(),
        };
        app.apply_workspaces(
            snapshot(vec![
                bare_workspace("w1", "alpha", false, Some(NOW - 60)),
                bare_workspace("w2", "beta", false, Some(NOW - 2 * 3_600)),
            ]),
            NOW,
        );
        app.selected = 1;

        app.apply_workspaces(snapshot(vec![]), NOW);

        assert_eq!(glance_titles(&app), ["alpha", "beta"]);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn workspace_filter_hides_directories_and_other_sources_leave_the_glance() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![bare_workspace("w1", "helm", true, Some(NOW - 60))],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        app.entries
            .push(entry(Source::Zoxide, "/work/helm-dir", "helm-dir"));
        app.apply_filter();
        assert_eq!(glance_titles(&app), ["helm"]);

        app.source_filter = Some(Source::Workspace);
        app.query = "helm".into();
        app.apply_filter();
        assert_eq!(glance_titles(&app), ["helm"]);

        app.source_filter = Some(Source::Zoxide);
        app.query.clear();
        app.apply_filter();
        assert!(!app.glance_view());
        assert_eq!(glance_titles(&app), ["helm-dir"]);
    }

    #[test]
    fn agent_query_tokens_match_workspaces_by_their_agent_panes() {
        let mut helm = bare_workspace("w1", "helm", true, Some(NOW - 60));
        helm.tabs = vec![TabNode {
            id: "w1:t1".into(),
            label: "Code".into(),
            focused: true,
            panes: vec![PaneNode {
                id: "p1".into(),
                label: "p1".into(),
                cwd: PathBuf::from("/repos/helm"),
                focused: false,
                title: None,
                agent_session: None,
                agent: Some(AgentMeta {
                    name: "claude".into(),
                    state: AgentState::Working,
                }),
            }],
        }];
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![helm, bare_workspace("w2", "skills", false, Some(NOW - 60))],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );

        for (query, expected) in [
            ("agent", vec!["helm"]),
            ("!claude", vec!["helm"]),
            ("!codex", vec![]),
            ("#working", vec!["helm"]),
            ("@working", vec!["helm"]),
            ("claude", vec![]),
            ("working", vec![]),
        ] {
            app.query = query.into();
            app.apply_filter();
            assert_eq!(glance_titles(&app), expected, "query {query:?}");
        }
    }

    #[test]
    fn quick_actions_and_plugins_join_the_other_group_only_while_searching() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![bare_workspace("w1", "helm", true, Some(NOW - 60))],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        app.entries
            .push(entry(Source::QuickAction, "/quick/notes", "Quick: notes"));
        app.apply_filter();
        assert_eq!(glance_titles(&app), ["helm"]);

        app.query = "notes".into();
        app.apply_filter();
        assert_eq!(glance_titles(&app), ["Quick: notes"]);
        assert_eq!(
            app.glance_group(&app.entries[app.filtered[0]]),
            Some(Group::Other)
        );
    }

    fn repo_workspace(id: &str, repo: &str, group_age: i64) -> WorkspaceNode {
        let name = repo.rsplit('/').next().unwrap();
        let mut workspace = bare_workspace(id, name, false, Some(NOW - group_age));
        if let Some(git) = workspace.git.as_mut() {
            git.repo_key = repo.into();
        }
        workspace.tabs = vec![TabNode {
            id: format!("{id}:t1"),
            label: "Code".into(),
            focused: true,
            panes: vec![PaneNode {
                id: format!("{id}:p1"),
                label: "p1".into(),
                cwd: PathBuf::from(format!("/Users/brunner/repos/{repo}")),
                focused: false,
                title: None,
                agent_session: None,
                agent: Some(AgentMeta {
                    name: "claude".into(),
                    state: AgentState::Idle,
                }),
            }],
        }];
        workspace
    }

    fn directory(source: Source, path: &str) -> Entry {
        let mut entry = entry(source, path, path.rsplit('/').next().unwrap());
        entry.subtitle = path.into();
        entry
    }

    fn search_app(workspaces: Vec<WorkspaceNode>, directories: Vec<Entry>) -> App {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology { workspaces },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        app.entries.extend(directories);
        app.apply_filter();
        app
    }

    fn search(app: &mut App, query: &str) {
        app.query = query.into();
        app.apply_filter();
    }

    fn owner_glance() -> App {
        search_app(
            vec![
                repo_workspace("w1", "black-atom-industries/black-atom", 60),
                repo_workspace("w2", "black-atom-industries/helm.herdr", 120),
                repo_workspace("w3", "black-atom-industries/iter.nvim", 2 * 3_600),
                repo_workspace("w4", "black-atom-industries/radar.nvim", 9 * 86_400),
                repo_workspace("w5", "nikbrunner/mdn.nvim", 9 * 86_400),
            ],
            vec![
                directory(Source::Zoxide, "/Users/brunner/repos/nikbrunner/lager"),
                directory(
                    Source::Zoxide,
                    "/Users/brunner/repos/imfusion/cp/labels-web/.git",
                ),
                directory(
                    Source::Zoxide,
                    "/Users/brunner/repos/black-atom-industries/shiplog",
                ),
                directory(
                    Source::Zoxide,
                    "/Users/brunner/repos/nikbrunner/dots/common/.agents/skills",
                ),
                directory(
                    Source::Zoxide,
                    "/Users/brunner/repos/nikbrunner/dots/common/.agents/skills/nbr-plan",
                ),
                directory(Source::Zoxide, "/Users/brunner/.config/nvim"),
            ],
        )
    }

    fn selected_title(app: &App) -> &str {
        &app.selected_entry().unwrap().title
    }

    #[test]
    fn glance_search_drops_loose_fuzzy_matches_far_below_the_best() {
        let mut app = owner_glance();

        search(&mut app, "lag");
        assert_eq!(glance_titles(&app), ["lager"]);
        assert_eq!(selected_title(&app), "lager");

        search(&mut app, "lager");
        assert_eq!(glance_titles(&app), ["lager"]);
    }

    #[test]
    fn broad_glance_search_keeps_every_close_match() {
        let mut app = owner_glance();
        search(&mut app, "nvim");
        assert_eq!(
            glance_titles(&app),
            ["iter.nvim", "radar.nvim", "mdn.nvim", "nvim"]
        );
    }

    #[test]
    fn query_tokens_without_text_keep_every_matching_row() {
        let mut app = owner_glance();
        search(&mut app, "#idle");
        assert_eq!(glance_titles(&app).len(), 5);
    }

    #[test]
    fn glance_search_selects_the_best_match_as_the_query_changes() {
        let mut app = search_app(
            vec![
                repo_workspace("w1", "black-atom-industries/helm.herdr", 60),
                repo_workspace("w2", "nikbrunner/herdr-plus", 9 * 86_400),
            ],
            vec![],
        );

        search(&mut app, "herdr");
        assert_eq!(glance_titles(&app), ["helm.herdr", "herdr-plus"]);
        assert_eq!(selected_title(&app), "herdr-plus");

        search(&mut app, "helm");
        assert_eq!(selected_title(&app), "helm.herdr");
    }

    #[test]
    fn a_moved_selection_stays_on_its_row_while_it_is_visible() {
        let mut app = search_app(
            vec![
                repo_workspace("w1", "black-atom-industries/helm.herdr", 60),
                repo_workspace("w2", "nikbrunner/herdr-plus", 9 * 86_400),
            ],
            vec![],
        );
        search(&mut app, "herdr");
        app.prev();
        assert_eq!(selected_title(&app), "helm.herdr");

        search(&mut app, "herd");
        assert_eq!(selected_title(&app), "helm.herdr");

        search(&mut app, "plus");
        assert_eq!(glance_titles(&app), ["herdr-plus"]);
        assert_eq!(selected_title(&app), "herdr-plus");

        search(&mut app, "herdr");
        assert_eq!(selected_title(&app), "herdr-plus");
    }

    #[test]
    fn near_best_keeps_rows_from_85_percent_of_the_best_score() {
        assert!(near_best(72, 84));
        assert!(!near_best(71, 84));
    }

    #[test]
    fn glance_search_keeps_word_start_hits_for_a_single_letter() {
        let mut app = owner_glance();
        search(&mut app, "l");
        assert_eq!(glance_titles(&app), [".git", "lager"]);

        search(&mut app, "dots");
        assert_eq!(glance_titles(&app), ["skills", "nbr-plan"]);
    }

    #[test]
    fn query_tokens_keep_working_on_flat_lists() {
        let mut app = owner_glance();
        app.set_filter(Some(Source::Zoxide));
        search(&mut app, "/.config");
        assert_eq!(glance_titles(&app), ["nvim"]);
        search(&mut app, "#idle");
        assert!(glance_titles(&app).is_empty());
        search(&mut app, "!claude");
        assert!(glance_titles(&app).is_empty());
    }

    #[test]
    fn opening_a_row_that_needs_you_focuses_the_waiting_pane() {
        struct Recorder(std::rc::Rc<std::cell::RefCell<Vec<EntryAction>>>);
        impl HerdrControl for Recorder {
            fn focus(&self, action: &EntryAction) -> Result<(), String> {
                self.0.borrow_mut().push(action.clone());
                Ok(())
            }
        }

        let _env = command_test_env();
        let mut app = search_app(
            vec![
                agent_workspace(None, "w1", "api", AgentState::Blocked),
                agent_workspace(None, "w2", "web", AgentState::Idle),
            ],
            Vec::new(),
        );
        app.config.notifications.enabled = false;
        let focused = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        app.herdr_control = Some(Box::new(Recorder(focused.clone())));
        for title in ["api", "web"] {
            app.selected = app
                .filtered
                .iter()
                .position(|index| app.entries[*index].title == title)
                .unwrap();
            app.open_selected(false).unwrap();
        }
        let focused = focused.borrow();
        assert!(
            matches!(&focused[0], EntryAction::FocusPane { session: None, id } if id == "w1:p1")
        );
        assert!(
            matches!(&focused[1], EntryAction::FocusWorkspace { session: None, id } if id == "w2")
        );
    }

    fn agent_workspace(
        session: Option<&str>,
        id: &str,
        label: &str,
        state: AgentState,
    ) -> WorkspaceNode {
        let mut workspace = bare_workspace(id, label, false, None);
        workspace.session = session.map(str::to_string);
        workspace.tabs = vec![TabNode {
            id: format!("{id}:t1"),
            label: "1".into(),
            focused: true,
            panes: vec![PaneNode {
                id: format!("{id}:p1"),
                label: "1".into(),
                cwd: PathBuf::from(format!("/repos/{label}")),
                focused: true,
                title: None,
                agent: Some(AgentMeta {
                    name: "claude".into(),
                    state,
                }),
                agent_session: None,
            }],
        }];
        workspace
    }

    #[test]
    fn other_sessions_join_the_glance_only_where_they_need_you() {
        let mut waiting = agent_workspace(Some("work"), "w1", "api", AgentState::Blocked);
        waiting.focused = true;
        let mut app = App::new(Config::default(), Theme::terminal());
        app.pinned_entries
            .insert(topology_workspace_pin_key(Some("work"), "w1"));
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        bare_workspace("w1", "helm", true, Some(NOW - 60)),
                        waiting,
                        agent_workspace(Some("work"), "w2", "notes", AgentState::Idle),
                        agent_workspace(Some("work"), "w3", "docs", AgentState::Working),
                    ],
                },
                activity: ActivityState::default(),
                previous: Some("w1".into()),
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );

        assert_eq!(glance_titles(&app), ["api", "helm"]);
        let api = app.entries[app.filtered[0]].clone();
        let row = app.glance_row(&api).unwrap();
        assert_eq!(row.group, Group::NeedsYou);
        assert_eq!(row.mark, Mark::None);
        assert_eq!(row.other_session.as_deref(), Some("work"));
        assert!(app.is_elsewhere(&api));
        let helm = app.entries[app.filtered[1]].clone();
        assert!(!app.is_elsewhere(&helm));
        assert_eq!(app.glance_row(&helm).unwrap().mark, Mark::Here);
    }

    #[test]
    fn a_star_query_searches_the_workspaces_of_every_session() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        bare_workspace("w1", "helm", true, Some(NOW - 60)),
                        agent_workspace(Some("work"), "w2", "notes", AgentState::Idle),
                        agent_workspace(Some("lab"), "w3", "docs", AgentState::Working),
                    ],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        assert_eq!(glance_titles(&app), ["helm"]);

        for (query, expected) in [
            ("notes", vec![]),
            ("*", vec!["helm", "notes", "docs"]),
            ("* notes", vec!["notes"]),
            ("*work", vec!["notes"]),
            ("*default", vec!["helm"]),
        ] {
            app.query = query.into();
            app.apply_filter();
            let mut titles = glance_titles(&app);
            titles.sort();
            let mut expected: Vec<String> = expected.into_iter().map(String::from).collect();
            expected.sort();
            assert_eq!(titles, expected, "query {query:?}");
        }
        app.query = "*work".into();
        app.apply_filter();
        let notes = app.entries[app.filtered[0]].clone();
        assert_eq!(app.elsewhere_session(&notes), Some("work"));
    }

    #[test]
    fn the_default_session_is_elsewhere_from_a_named_one() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        agent_workspace(Some("work"), "w1", "helm", AgentState::Idle),
                        agent_workspace(None, "w1", "home", AgentState::Done),
                    ],
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: HashMap::new(),
                session: Some("work".into()),
                sessions: Vec::new(),
            },
            NOW,
        );
        let home = app
            .entries
            .iter()
            .find(|entry| entry.title == "home")
            .unwrap();
        assert_eq!(
            app.glance_row(home).unwrap().other_session.as_deref(),
            Some("default")
        );
    }

    #[test]
    fn only_other_session_workspaces_that_need_you_are_read_further() {
        let mut topology = OpenTopology {
            workspaces: vec![bare_workspace("w1", "helm", true, None)],
        };
        let others = OpenTopology {
            workspaces: vec![
                agent_workspace(Some("work"), "w1", "api", AgentState::Done),
                agent_workspace(Some("work"), "w2", "notes", AgentState::Idle),
            ],
        };
        join_needs_you(&mut topology, others, &ActivityState::default());
        let labels: Vec<&str> = topology
            .workspaces
            .iter()
            .map(|workspace| workspace.label.as_str())
            .collect();
        assert_eq!(labels, ["helm", "api"]);
    }

    fn idle_since(session: &str, since: i64, seen_at: Option<i64>) -> ActivityState {
        let mut activity = ActivityState::default();
        activity.record_agent_status(Some(session), "w1:p1", "idle", since);
        if let Some(seen_at) = seen_at {
            activity.record_seen(Some(session), seen_at);
        }
        activity
    }

    #[test]
    fn an_agent_elsewhere_that_went_idle_after_the_user_left_counts_as_done() {
        let workspace = agent_workspace(Some("work"), "w1", "api", AgentState::Idle);
        let state = |activity: &ActivityState, elsewhere| {
            glance::needs_you_state(&glance::evidence(
                &workspace, activity, false, None, elsewhere,
            ))
        };
        let unseen = idle_since("work", NOW, Some(NOW - 1));
        assert_eq!(state(&unseen, true), Some(AgentState::Done));
        assert_eq!(state(&unseen, false), None);
        assert_eq!(state(&idle_since("work", NOW, Some(NOW)), true), None);
        assert_eq!(state(&idle_since("work", NOW, None), true), None);

        let mut focused = idle_since("work", NOW, None);
        focused.record_focus(Some("work"), "w2", NOW - 1);
        focused.record_focus(Some("play"), "w1", NOW + 1);
        assert_eq!(state(&focused, true), Some(AgentState::Done));
        focused.record_focus(Some("work"), "w3", NOW);
        assert_eq!(state(&focused, true), None);

        let mut topology = OpenTopology::default();
        join_needs_you(
            &mut topology,
            OpenTopology {
                workspaces: vec![workspace.clone()],
            },
            &unseen,
        );
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: vec![
                        bare_workspace("w1", "helm", true, None),
                        topology.workspaces[0].clone(),
                    ],
                },
                activity: unseen,
                previous: None,
                subagents: HashMap::new(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
        let api = app
            .entries
            .iter()
            .find(|entry| entry.title == "api")
            .unwrap();
        let row = app.glance_row(api).unwrap();
        assert_eq!(row.group, Group::NeedsYou);
        assert_eq!(row.state, AgentState::Done);
    }

    #[test]
    fn opening_helm_and_focus_events_record_the_current_session_as_seen() {
        let _env = command_test_env();
        let before = now_unix_secs();
        record_seen().unwrap();
        assert!(ActivityState::load().seen_at(Some("work")) >= Some(before));

        ActivityState::update(|activity| activity.record_seen(Some("work"), 1)).unwrap();
        env::set_var(
            "HERDR_PLUGIN_EVENT_JSON",
            r#"{"data":{"workspace_id":"w1"}}"#,
        );
        track_focus(&command_test_config()).unwrap();
        let activity = ActivityState::load();
        assert!(activity.seen_at(Some("work")) >= Some(before));
        assert_eq!(activity.seen_at(None), None);
    }

    #[test]
    fn a_failed_local_read_keeps_the_last_good_list_even_with_other_sessions() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let load = |workspaces| Workspaces {
            topology: OpenTopology { workspaces },
            activity: ActivityState::default(),
            previous: None,
            subagents: HashMap::new(),
            session: None,
            sessions: Vec::new(),
        };
        app.apply_workspaces(load(vec![bare_workspace("w1", "helm", true, None)]), NOW);
        app.apply_workspaces(
            load(vec![agent_workspace(
                Some("work"),
                "w1",
                "api",
                AgentState::Blocked,
            )]),
            NOW,
        );
        assert_eq!(glance_titles(&app), ["helm"]);
    }
}
