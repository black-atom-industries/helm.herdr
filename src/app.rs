use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{
    config::Config,
    herdr::{herdr_json, herdr_json_args, notify_done, notify_error, run_herdr},
    integrations::{command, herdr_plus, sessions},
    matcher::match_score,
    model::{Entry, EntryAction, Source, WorkspaceKind, WorkspaceRef},
    paths::{canonical_str, herdr_plus_quick_actions_dir, home, plugin_config_dir},
    recent::RecentState,
    sources::{collect_agents, collect_open_topology, collect_roots, collect_zoxide},
    theme::Theme,
    topology::{self, ChildSelection, OpenTopology, TopologyCursor, TopologyDepth},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputMode {
    Normal,
    Search,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionKind {
    Topology,
    Results,
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
    pub(crate) recent_state: RecentState,
    pub(crate) spinner_tick: u32,
    pub(crate) update_available: Option<String>,
    pub(crate) topology: OpenTopology,
    pub(crate) topology_entries: Vec<Entry>,
    pub(crate) topology_cursor: TopologyCursor,
    pub(crate) topology_memory: HashMap<String, ChildSelection>,
    #[cfg(test)]
    pub(crate) herdr_control: Option<Box<dyn HerdrControl>>,
    initial_selection_pending: bool,
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
            recent_state: RecentState::default(),
            spinner_tick: 0,
            update_available: None,
            topology: OpenTopology::default(),
            topology_entries: Vec::new(),
            topology_cursor: TopologyCursor::default(),
            topology_memory: HashMap::new(),
            #[cfg(test)]
            herdr_control: None,
            initial_selection_pending: true,
        }
    }

    pub(crate) fn refresh(&mut self) {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        let (open_entries, workspace_entries, path_to_workspaces) =
            collect_open_topology(self.config.sources.open_workspaces);
        self.path_to_workspaces = path_to_workspaces;
        self.topology = if self.config.sources.open_workspaces {
            let session = current_session_name();
            let mut topology = topology::collect_topology(
                |args| herdr_json_args(args.iter().copied()),
                session,
                &self.config.agent_aliases,
                self.config.sources.agents,
            );
            topology::enrich_git(&mut topology, &topology::NativeGitProbe);
            topology
        } else {
            OpenTopology::default()
        };
        self.recent_state = RecentState::load();
        if self.config.sources.open_workspaces {
            normalize_recent_state(&mut self.recent_state, &open_entries);
            self.recent_state.save();
        }

        if self.config.sources.herdr_plus_projects {
            push_unique(&mut entries, &mut seen, herdr_plus::collect_projects());
        }
        if self.config.sources.zoxide {
            push_unique(&mut entries, &mut seen, collect_zoxide());
        }
        if self.config.sources.roots {
            push_unique(&mut entries, &mut seen, collect_roots(&self.config));
        }
        if self.config.sources.servers {
            push_unique(
                &mut entries,
                &mut seen,
                sessions::collect_remotes(&self.config),
            );
        }
        if self.config.sources.agents {
            entries.extend(collect_agents(
                &workspace_entries,
                &self.config.agent_aliases,
            ));
        }
        if self.config.sources.herdr_plus_quick_actions && herdr_plus_quick_actions_dir().is_dir() {
            entries.push(herdr_plus::quick_actions_entry());
        }
        push_unique(
            &mut entries,
            &mut seen,
            command::collect(&self.config.integrations),
        );

        self.topology_entries = topology::query_entries(&self.topology, self.config.sources.agents);
        let topology_agent_targets = self
            .topology_entries
            .iter()
            .filter_map(|entry| {
                matches!(entry.action, EntryAction::FocusPane { .. })
                    .then(|| entry.agent_target.as_deref())
                    .flatten()
            })
            .collect::<HashSet<_>>();
        entries.retain(|entry| {
            !matches!(&entry.action, EntryAction::FocusAgent { target } if topology_agent_targets.contains(target.as_str()))
        });
        entries.extend(self.topology_entries.iter().cloned());
        self.entries = entries;
        self.pinned_entries =
            read_pinned_entries(&plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE))
                .unwrap_or_default();
        if migrate_legacy_topology_pins(&self.entries, &mut self.pinned_entries) {
            let _ = save_pinned_entries(
                &plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE),
                &self.pinned_entries,
            );
        }
        self.previous_workspace_id =
            if self.config.jump_back.enabled && self.config.jump_back.pin_previous {
                read_previous_workspace().ok()
            } else {
                None
            };
        self.sort_topology();
        self.sync_topology_cursor();
        self.apply_filter();
    }

    pub(crate) fn projection_kind(&self) -> ProjectionKind {
        if self.query.trim().is_empty()
            && self
                .source_filter
                .as_ref()
                .is_none_or(|source| *source == Source::Workspace)
        {
            ProjectionKind::Topology
        } else {
            ProjectionKind::Results
        }
    }

    pub(crate) fn topology_view(&self) -> bool {
        self.projection_kind() == ProjectionKind::Topology
    }

    fn topology_workspace_key(workspace: &crate::topology::WorkspaceNode) -> String {
        format!(
            "{}::{}",
            workspace.session.as_deref().unwrap_or("<default>"),
            workspace.id
        )
    }

    fn sort_topology(&mut self) {
        let previous = self.previous_workspace_id.as_deref();
        self.topology.workspaces.sort_by_key(|workspace| {
            let current = workspace.focused;
            let is_previous = !current && previous == Some(workspace.id.as_str());
            let recent = self
                .recent_state
                .recent_ids(workspace.session.as_deref())
                .iter()
                .position(|id| id == &workspace.id)
                .unwrap_or(usize::MAX);
            (!current, !is_previous, recent)
        });
    }

    pub(crate) fn sync_topology_cursor(&mut self) {
        let live = self
            .topology
            .workspaces
            .iter()
            .map(Self::topology_workspace_key)
            .collect::<HashSet<_>>();
        self.topology_memory.retain(|key, _| live.contains(key));
        self.topology_cursor = TopologyCursor::new(&self.topology);
        for (index, workspace) in self.topology.workspaces.iter().enumerate() {
            if let Some(selection) = self
                .topology_memory
                .get(&Self::topology_workspace_key(workspace))
            {
                self.topology_cursor.selection[index] = *selection;
            }
        }
        self.topology_cursor.clamp(&self.topology);
    }

    pub(crate) fn remember_topology_selection(&mut self) {
        if let Some(workspace) = self.topology.workspaces.get(self.topology_cursor.workspace) {
            self.topology_memory.insert(
                Self::topology_workspace_key(workspace),
                self.topology_cursor.selection[self.topology_cursor.workspace],
            );
        }
    }

    pub(crate) fn topology_move_vertical(&mut self, delta: isize) {
        if !self.topology_view() {
            if delta.is_negative() {
                self.prev();
            } else {
                self.next();
            }
            return;
        }
        self.remember_topology_selection();
        match (self.topology_cursor.depth, delta) {
            (TopologyDepth::Workspace, _) => {
                self.topology_cursor.move_workspace(&self.topology, delta)
            }
            (TopologyDepth::Tab, d) if d.is_negative() => self.topology_cursor.leave_to_workspace(),
            (TopologyDepth::Tab, _) => self.topology_cursor.enter_pane(&self.topology),
            (TopologyDepth::Pane, d) if d.is_negative() => self.topology_cursor.leave_to_tab(),
            (TopologyDepth::Pane, d) if d.is_positive() => {
                let workspace = self.topology_cursor.workspace;
                self.topology_cursor.move_workspace(&self.topology, 1);
                if self.topology_cursor.workspace != workspace {
                    self.topology_cursor.leave_to_tab();
                }
            }
            (TopologyDepth::Pane, _) => {}
        }
        self.topology_cursor.clamp(&self.topology);
    }

    pub(crate) fn topology_move_horizontal(&mut self, delta: isize) {
        if !self.topology_view() {
            return;
        }
        self.remember_topology_selection();
        match self.topology_cursor.depth {
            TopologyDepth::Workspace if delta.is_positive() => {
                self.topology_cursor.enter_tab(&self.topology)
            }
            TopologyDepth::Workspace => {}
            TopologyDepth::Tab => self.topology_cursor.move_tab(&self.topology, delta),
            TopologyDepth::Pane => self.topology_cursor.move_pane(&self.topology, delta),
        }
        self.topology_cursor.clamp(&self.topology);
    }

    pub(crate) fn topology_move_workspace(&mut self, delta: isize) {
        if !self.topology_view() {
            return;
        }
        self.remember_topology_selection();
        self.topology_cursor.move_workspace(&self.topology, delta);
        self.topology_cursor.clamp(&self.topology);
    }

    pub(crate) fn topology_selected_entry(&self) -> Option<&Entry> {
        let workspace = self
            .topology
            .workspaces
            .get(self.topology_cursor.workspace)?;
        let entry = match self.topology_cursor.depth {
            TopologyDepth::Workspace => self
                .topology_entries
                .iter()
                .find(|entry| matches!(&entry.action, EntryAction::FocusWorkspace { session, id } if id == &workspace.id && session == &workspace.session)),
            TopologyDepth::Tab => {
                let tab = workspace.tabs.get(self.topology_cursor.selection[self.topology_cursor.workspace].tab)?;
                self.topology_entries.iter().find(|entry| matches!(&entry.action, EntryAction::FocusTab { session, id } if id == &tab.id && session == &workspace.session))
            }
            TopologyDepth::Pane => {
                let selected = self.topology_cursor.selection[self.topology_cursor.workspace];
                let pane = workspace.tabs.get(selected.tab)?.panes.get(selected.pane)?;
                self.topology_entries.iter().find(|entry| matches!(&entry.action, EntryAction::FocusPane { session, id } if id == &pane.id && session == &workspace.session))
            }
        }?;
        Some(entry)
    }

    pub(crate) fn apply_filter(&mut self) {
        let query = Query::parse(&self.query);
        let empty_query = query.plain.is_empty();
        let searching = !self.query.trim().is_empty();
        let agent_view =
            query.all_agents || (self.source_filter == Some(Source::Agent) && empty_query);
        let use_agent_priority = empty_query
            && (agent_view || self.source_filter.is_none())
            && agent_sort(&self.config.picker.agent_sort) == "priority";
        let pin_previous = self.config.jump_back.enabled
            && self.config.jump_back.pin_previous
            && !searching
            && self.source_filter.is_none();

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
                if *source == Source::Agent {
                    if !is_agent_entry(entry) {
                        continue;
                    }
                } else if &entry.source != source {
                    continue;
                }
            }
            if !query.filters_match(entry) {
                continue;
            }
            let bonus = self.config.picker.source_bonus(&entry.source)
                + query.score_bonus(entry, use_agent_priority);
            if query.plain.is_empty() {
                scored.push((bonus, idx));
            } else if let Some(score) =
                match_score(&self.config.picker.engine, &entry.haystack(), &query.plain)
            {
                scored.push((score + bonus, idx));
            }
        }
        scored.sort_by(|(score_a, idx_a), (score_b, idx_b)| {
            let user_pinned_a = self.is_pinned(&self.entries[*idx_a]);
            let user_pinned_b = self.is_pinned(&self.entries[*idx_b]);
            let previous_pinned_a = pin_previous
                && self.entries[*idx_a].source == Source::Workspace
                && self.entries[*idx_a].workspace_id.as_deref()
                    == self.previous_workspace_id.as_deref();
            let previous_pinned_b = pin_previous
                && self.entries[*idx_b].source == Source::Workspace
                && self.entries[*idx_b].workspace_id.as_deref()
                    == self.previous_workspace_id.as_deref();
            previous_pinned_b
                .cmp(&previous_pinned_a)
                .then_with(|| user_pinned_b.cmp(&user_pinned_a))
                .then_with(|| score_b.cmp(score_a))
                .then_with(|| {
                    self.config
                        .picker
                        .source_rank(&self.entries[*idx_a].source)
                        .cmp(&self.config.picker.source_rank(&self.entries[*idx_b].source))
                })
                .then_with(|| idx_a.cmp(idx_b))
        });

        self.filtered = Vec::new();
        self.filtered_scores = Vec::new();
        for (score, index) in scored {
            self.filtered.push(index);
            self.filtered_scores.push(score);
        }
        self.selected = if searching {
            self.best_search_position(&query, use_agent_priority)
                .unwrap_or(0)
        } else {
            0
        };
        if self.initial_selection_pending {
            self.initial_selection_pending = false;
            if self.query.trim().is_empty() && self.source_filter.is_none() {
                if let Some(position) = self.filtered.iter().position(|index| {
                    matches!(
                        self.entries[*index].action,
                        EntryAction::FocusWorkspace { .. }
                    )
                }) {
                    self.selected = position;
                }
            }
        }
    }

    fn best_search_position(&self, query: &Query, use_agent_priority: bool) -> Option<usize> {
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
                    match_score(&self.config.picker.engine, &entry.haystack(), &query.plain)?
                };
                let score = score
                    + self.config.picker.source_bonus(&entry.source)
                    + query.score_bonus(entry, use_agent_priority);
                Some((score, position))
            })
            .max_by(|(score_a, position_a), (score_b, position_b)| {
                score_a
                    .cmp(score_b)
                    .then_with(|| position_b.cmp(position_a))
            })
            .map(|(_, position)| position)
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
        let entry = if self.topology_view() {
            self.topology_selected_entry()
        } else {
            self.selected_entry()
        }
        .ok_or("nothing selected")?;
        let key = pin_key(entry);
        let legacy_key = legacy_current_workspace_pin_key(entry);
        let mut pinned = self.pinned_entries.clone();
        let removed_primary = pinned.remove(&key);
        let removed_legacy = legacy_key
            .as_ref()
            .is_some_and(|legacy| pinned.remove(legacy));
        if !(removed_primary || removed_legacy) {
            pinned.insert(key);
        }
        save_pinned_entries(
            &plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE),
            &pinned,
        )?;
        self.pinned_entries = pinned;
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

    pub(crate) fn open_selected(&mut self, use_directory_template: bool) -> Result<(), String> {
        let e = if self.topology_view() {
            self.topology_selected_entry().cloned()
        } else {
            self.selected_entry().cloned()
        }
        .ok_or("nothing selected")?;
        let action_destination = recent_destination_for_entry(&e);
        let tracks_workspace_transition =
            self.config.jump_back.enabled && tracks_workspace_transition(&e.action);
        let origin_workspace = if tracks_workspace_transition {
            launch_workspace_id().or_else(|| current_workspace_id().ok())
        } else {
            None
        };
        let (result, notify_success, notify_failure) = match &e.action {
            EntryAction::FocusAgent { target } => {
                (run_herdr(["agent", "focus", target]), true, true)
            }
            EntryAction::FocusPane { .. }
            | EntryAction::FocusWorkspace { .. }
            | EntryAction::FocusTab { .. } => (self.focus_action(&e.action), true, true),
            EntryAction::OpenProject => (self.open_project(&e), true, true),
            EntryAction::OpenRemote { target } => (sessions::open_remote(target), false, true),
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
                if tracks_workspace_transition {
                    let destination = current_workspace_id().ok();
                    if let Some(previous) = previous_workspace_to_record(
                        origin_workspace.as_deref(),
                        destination.as_deref(),
                    ) {
                        let _ = save_previous_workspace(previous);
                    }
                }
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
            self.record_recent_workspace(current_session_name().as_deref(), &destination);
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
            Source::Agent => e.workspace_id.clone(),
            Source::Project => self.matching_project_workspace(e).map(|ws| ws.id.clone()),
            Source::Zoxide | Source::Root => self.matching_dir_workspace(e).map(|ws| ws.id.clone()),
            Source::Server | Source::Session | Source::QuickAction | Source::Integration => None,
        }
    }

    pub(crate) fn open_project(&self, e: &Entry) -> Result<(), String> {
        if self.config.picker.reuse_existing {
            if let Some(ws) = self.matching_project_workspace(e) {
                return run_herdr(["workspace", "focus", &ws.id]);
            }
        }
        if !self.config.picker.create_missing {
            return Err("create_missing=false and no workspace exists".into());
        }
        let project = e.project.as_ref();
        let label = project
            .map(|p| p.name.clone())
            .unwrap_or_else(|| e.title.clone());
        let json = herdr_json([
            "workspace",
            "create",
            "--cwd",
            &e.path.display().to_string(),
            "--label",
            &label,
            "--focus",
        ])?;
        if let Some(p) = project {
            herdr_plus::bootstrap_project_tabs(p, &json, &e.path)?;
        }
        Ok(())
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

    pub(crate) fn workspaces_for_entry(&self, e: &Entry) -> &[WorkspaceRef] {
        self.path_to_workspaces
            .get(&e.key())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn matching_project_workspace(&self, e: &Entry) -> Option<&WorkspaceRef> {
        let workspaces = self.workspaces_for_entry(e);
        workspaces
            .iter()
            .find(|ws| ws.kind == WorkspaceKind::Project)
            .or_else(|| {
                workspaces
                    .iter()
                    .find(|ws| ws.kind == WorkspaceKind::Unknown)
            })
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

fn current_session_name() -> Option<String> {
    env::var("HERDR_SESSION")
        .ok()
        .filter(|name| !name.trim().is_empty())
}

fn tracks_workspace_transition(action: &EntryAction) -> bool {
    matches!(
        action,
        EntryAction::FocusAgent { .. }
            | EntryAction::FocusPane { .. }
            | EntryAction::FocusWorkspace { .. }
            | EntryAction::FocusTab { .. }
            | EntryAction::OpenProject
            | EntryAction::FocusOrCreateDir
    )
}

fn recent_destination_for_entry(e: &Entry) -> Option<(Option<String>, String)> {
    match &e.action {
        EntryAction::FocusWorkspace { session, id, .. } => Some((session.clone(), id.clone())),
        EntryAction::FocusTab { session, .. } => e
            .workspace_id
            .as_ref()
            .map(|workspace_id| (session.clone(), workspace_id.clone())),
        EntryAction::FocusAgent { .. } => e
            .workspace_id
            .as_ref()
            .map(|workspace_id| (current_session_name(), workspace_id.clone())),
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
    matches!(
        action,
        EntryAction::FocusAgent { .. } | EntryAction::OpenProject | EntryAction::FocusOrCreateDir
    )
    .then(|| {
        resulting_workspace_id.map(|workspace_id| (current_session_name(), workspace_id.into()))
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

    fn score_bonus(&self, entry: &Entry, use_agent_priority: bool) -> i64 {
        if entry.source == Source::Agent && use_agent_priority {
            agent_status_bonus(entry)
        } else {
            0
        }
    }
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
    entry.agent_target.is_some() || entry.source == Source::Agent
}

fn ordinary_source_label(entry: &Entry) -> &'static str {
    match entry.action {
        EntryAction::FocusWorkspace { .. } => "workspace",
        EntryAction::FocusTab { .. } => "tab",
        EntryAction::FocusPane { .. } | EntryAction::FocusAgent { .. } => "pane",
        _ => entry.source.label(),
    }
}

fn agent_status_bonus(entry: &Entry) -> i64 {
    let status = status_text(entry);
    if ["block", "fail", "error"]
        .iter()
        .any(|needle| status.contains(needle))
    {
        4
    } else if ["need", "attention", "review", "request", "question", "wait"]
        .iter()
        .any(|needle| status.contains(needle))
    {
        3
    } else if status.contains("done") || status.contains("complete") {
        2
    } else if status.contains("work") || status.contains("run") {
        1
    } else {
        0
    }
}

fn status_text(entry: &Entry) -> String {
    entry
        .search_terms
        .iter()
        .find_map(|term| term.strip_prefix("agent-status:"))
        .map(str::to_string)
        .unwrap_or_else(|| {
            entry
                .subtitle
                .split('·')
                .next()
                .unwrap_or(&entry.subtitle)
                .trim()
                .to_lowercase()
        })
}

fn agent_text(entry: &Entry) -> String {
    entry
        .title
        .split('·')
        .next()
        .unwrap_or(&entry.title)
        .to_string()
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
const PINNED_ENTRIES_STATE_FILE: &str = "pinned-entries.json";

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
    save_previous_workspace(&current)?;
    record_persisted_recent_workspace(current_session_name().as_deref(), &previous);
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

fn previous_workspace_to_record<'a>(
    origin: Option<&'a str>,
    destination: Option<&str>,
) -> Option<&'a str> {
    match (origin, destination) {
        (Some(origin), Some(destination)) if origin != destination => Some(origin),
        _ => None,
    }
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
    let path = plugin_config_dir().join(JUMP_BACK_STATE_FILE);
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
    let dir = plugin_config_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create plugin config: {e}"))?;
    let value = encode_previous_workspace(&current_session_identity(), id)?;
    fs::write(dir.join(JUMP_BACK_STATE_FILE), value)
        .map_err(|e| format!("failed to save jump-back state: {e}"))
}

fn read_pinned_entries(path: &Path) -> Result<HashSet<String>, String> {
    let value =
        fs::read_to_string(path).map_err(|e| format!("failed to read pinned entries: {e}"))?;
    serde_json::from_str::<Vec<String>>(&value)
        .map(|entries| entries.into_iter().collect())
        .map_err(|e| format!("failed to parse pinned entries: {e}"))
}

fn save_pinned_entries(path: &Path, entries: &HashSet<String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create plugin config: {e}"))?;
    }
    let mut entries = entries.iter().collect::<Vec<_>>();
    entries.sort_unstable();
    let value = serde_json::to_string_pretty(&entries)
        .map_err(|e| format!("failed to encode pinned entries: {e}"))?;
    fs::write(path, value).map_err(|e| format!("failed to save pinned entries: {e}"))
}

fn launch_workspace_id() -> Option<String> {
    env::var("HERDR_PLUGIN_CONTEXT_JSON")
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("workspace_id")?.as_str().map(str::to_string))
        .filter(|s| !s.is_empty())
}

fn agent_sort(configured: &str) -> String {
    match configured.to_lowercase().as_str() {
        "priority" => "priority".into(),
        "spaces" => "spaces".into(),
        _ => herdr_agent_panel_sort(),
    }
}

fn herdr_agent_panel_sort() -> String {
    let path = std::env::var("XDG_CONFIG_HOME")
        .map(|xdg| Path::new(&xdg).join("herdr/config.toml"))
        .unwrap_or_else(|_| home().join(".config/herdr/config.toml"));
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.parse::<toml::Value>().ok())
        .and_then(|v| {
            v.get("ui")
                .and_then(|x| x.as_table())
                .and_then(|x| x.get("agent_panel_sort"))
                .or_else(|| v.get("agent_panel_sort"))
                .and_then(|x| x.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "spaces".into())
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

fn pin_key(entry: &Entry) -> String {
    match &entry.action {
        EntryAction::FocusWorkspace { session, id, .. } => {
            topology_workspace_pin_key(session.as_deref(), id)
        }
        EntryAction::FocusTab { session, id, .. } => {
            format!("tab:{}:{id}", session_key(session.as_deref()))
        }
        EntryAction::FocusAgent { target } => format!("agent:{target}"),
        EntryAction::FocusPane { session, id } => {
            format!("pane:{}:{id}", session_key(session.as_deref()))
        }
        EntryAction::OpenProject => format!("project:{}", entry.key()),
        EntryAction::OpenRemote { target } => format!("remote:{target}"),
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
            EntryAction::OpenRemote { target } => format!("remote:{target}"),
            EntryAction::RunCommand { command, .. } => format!("{}:{command}", e.source_name()),
            _ if matches!(e.source, Source::Project | Source::Zoxide | Source::Root) => {
                format!("directory:{path_key}")
            }
            _ => format!("{}:{path_key}", e.source_name()),
        };
        if seen.insert(key) {
            entries.push(e);
        } else if e.source == Source::Root {
            if let Some(index) = entries.iter().position(|entry| {
                matches!(
                    entry.source,
                    Source::Project | Source::Zoxide | Source::Root
                ) && entry.key() == path_key
            }) {
                entries[index] = e;
            }
        }
    }
}

#[cfg(test)]
mod tests {
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

    use super::*;
    use crate::{
        config::{Config, JumpBackConfig},
        model::Project,
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
            project: None,
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
            project: None,
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
            project: None,
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
        config.sources.herdr_plus_projects = false;
        config.sources.zoxide = false;
        config.sources.roots = false;
        config.sources.agents = false;
        config.sources.servers = false;
        config.sources.sessions = false;
        config.sources.herdr_plus_quick_actions = false;
        config.jump_back.pin_previous = false;
        config
    }

    fn app_with_selected_entry(config: Config, entry: Entry) -> App {
        let mut app = App::new(config, Theme::terminal());
        app.entries = vec![entry];
        app.filtered = vec![0];
        app
    }

    #[test]
    fn initial_topology_selection_starts_at_workspace_depth() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.topology = OpenTopology {
            workspaces: vec![
                crate::topology::WorkspaceNode {
                    id: "w1".into(),
                    label: "web-ui".into(),
                    session: Some("dev".into()),
                    focused: true,
                    tabs: vec![
                        crate::topology::TabNode {
                            id: "t1".into(),
                            label: "Code".into(),
                            focused: false,
                            panes: vec![],
                        },
                        crate::topology::TabNode {
                            id: "t2".into(),
                            label: "Edit".into(),
                            focused: true,
                            panes: vec![],
                        },
                    ],
                    git: None,
                },
                crate::topology::WorkspaceNode {
                    id: "w2".into(),
                    label: "api".into(),
                    session: Some("dev".into()),
                    focused: false,
                    tabs: vec![],
                    git: None,
                },
            ],
        };

        app.sync_topology_cursor();

        assert_eq!(app.topology_cursor.depth, TopologyDepth::Workspace);
        assert_eq!(app.topology_cursor.selection[0].tab, 1);
        app.topology_move_vertical(1);
        assert_eq!(app.topology_cursor.workspace, 1);
    }

    #[test]
    fn first_query_character_projects_flat_direct_destinations() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.topology = OpenTopology {
            workspaces: vec![crate::topology::WorkspaceNode {
                id: "w1".into(),
                label: "web-ui".into(),
                session: Some("dev".into()),
                focused: true,
                tabs: vec![crate::topology::TabNode {
                    id: "t1".into(),
                    label: "Code".into(),
                    focused: true,
                    panes: vec![crate::topology::PaneNode {
                        id: "p1".into(),
                        label: "Shell".into(),
                        cwd: "/repo".into(),
                        focused: true,
                        title: None,
                        agent: None,
                    }],
                }],
                git: None,
            }],
        };
        app.topology_entries = topology::query_entries(&app.topology, false);
        app.entries = app.topology_entries.clone();
        app.query = "s".into();
        app.apply_filter();

        assert!(!app.topology_view());
        let selected = app.selected_entry().expect("flat result");
        assert_eq!(selected.source_name(), "pane");
        assert!(matches!(selected.action, EntryAction::FocusPane { .. }));
    }

    #[test]
    fn exact_agent_token_is_a_trait_predicate_and_agents_is_plain_text() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let mut agent = entry(Source::Agent, "/tmp", "Codex");
        agent.agent_target = Some("p1".into());
        let tab = entry(Source::Workspace, "/tmp", "Agents");
        app.entries = vec![agent, tab];

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
        let mut pane = entry(Source::Agent, "/tmp", "Shell");
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
        app.apply_filter();

        assert_eq!(app.filtered.len(), 1);
        assert_eq!(app.selected_entry().unwrap().source_name(), "bookmark");
    }

    #[test]
    fn previous_workspace_is_pinned_only_on_initial_unfiltered_view() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let mut alpha = entry(Source::Workspace, "/alpha", "alpha");
        alpha.workspace_id = Some("w1".into());
        alpha.action = EntryAction::FocusWorkspace {
            session: None,
            id: "w1".into(),
        };
        let mut zulu = entry(Source::Workspace, "/zulu", "zulu");
        zulu.workspace_id = Some("w2".into());
        zulu.action = EntryAction::FocusWorkspace {
            session: None,
            id: "w2".into(),
        };
        app.entries = vec![alpha, zulu];
        app.previous_workspace_id = Some("w2".into());

        app.apply_filter();
        assert_eq!(
            app.selected_entry().unwrap().workspace_id.as_deref(),
            Some("w2")
        );

        app.source_filter = Some(Source::Workspace);
        app.apply_filter();
        assert_eq!(
            app.selected_entry().unwrap().workspace_id.as_deref(),
            Some("w1")
        );

        app.source_filter = None;
        app.config.jump_back.pin_previous = false;
        app.apply_filter();
        assert_eq!(
            app.selected_entry().unwrap().workspace_id.as_deref(),
            Some("w1")
        );
    }

    #[test]
    fn equal_score_ties_preserve_insertion_order() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.entries = vec![
            entry(Source::Zoxide, "/zulu", "zulu"),
            entry(Source::Zoxide, "/alpha", "alpha"),
        ];

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

        app.apply_filter();

        assert_eq!(app.selected_entry().unwrap().title, "zulu");

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!("herdr-pins-{suffix}.json"));
        save_pinned_entries(&path, &app.pinned_entries).unwrap();
        assert_eq!(read_pinned_entries(&path).unwrap(), app.pinned_entries);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn previous_workspace_sorts_before_marked_entries() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let marked = entry(Source::Root, "/marked", "marked");
        let mut previous = entry(Source::Workspace, "/previous", "previous");
        previous.workspace_id = Some("w2".into());
        previous.action = EntryAction::FocusWorkspace {
            session: None,
            id: "w2".into(),
        };
        app.entries = vec![marked, previous];
        app.pinned_entries.insert(pin_key(&app.entries[0]));
        app.previous_workspace_id = Some("w2".into());

        app.apply_filter();

        assert_eq!(
            app.selected_entry().unwrap().workspace_id.as_deref(),
            Some("w2")
        );
    }

    #[test]
    fn source_specific_reuse_distinguishes_same_path_workspaces() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let mut project = entry(Source::Project, "/tmp", "tmp");
        project.project = Some(Project {
            name: "tmp".into(),
            description: String::new(),
            working_dir: "/tmp".into(),
            tabs: vec![],
        });
        let dir = entry(Source::Zoxide, "/tmp", "tmp");
        app.path_to_workspaces.insert(
            project.key(),
            vec![
                workspace("w1", "project: tmp", WorkspaceKind::Project, "/tmp"),
                workspace("w2", "dir: tmp", WorkspaceKind::Dir, "/tmp"),
            ],
        );

        assert_eq!(app.matching_project_workspace(&project).unwrap().id, "w1");
        assert_eq!(app.matching_dir_workspace(&dir).unwrap().id, "w2");
    }

    #[test]
    fn plain_workspace_is_shared_by_project_and_directory_entries() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let mut project = entry(Source::Project, "/tmp", "tmp");
        project.project = Some(Project {
            name: "tmp".into(),
            description: String::new(),
            working_dir: "/tmp".into(),
            tabs: vec![],
        });
        let dir = entry(Source::Zoxide, "/tmp", "tmp");
        app.path_to_workspaces.insert(
            project.key(),
            vec![workspace("w1", "tmp", WorkspaceKind::Unknown, "/tmp")],
        );

        assert_eq!(app.matching_project_workspace(&project).unwrap().id, "w1");
        assert_eq!(app.matching_dir_workspace(&dir).unwrap().id, "w1");
        assert_eq!(app.workspace_to_close(&project), Some("w1".into()));
        assert_eq!(app.workspace_to_close(&dir), Some("w1".into()));
    }

    #[test]
    fn offers_directory_template_for_new_and_existing_directory_workspaces() {
        let mut config = Config::default();
        config.picker.directory_template = Some("default.toml".into());
        let mut app = App::new(config, Theme::terminal());
        app.entries = vec![entry(Source::Zoxide, "/tmp", "tmp")];
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
        let mut project = entry(Source::Project, "/tmp", "tmp");
        project.project = Some(Project {
            name: "tmp".into(),
            description: String::new(),
            working_dir: "/tmp".into(),
            tabs: vec![],
        });
        let dir = entry(Source::Root, "/tmp", "tmp");
        app.path_to_workspaces.insert(
            project.key(),
            vec![
                workspace("w1", "project: tmp", WorkspaceKind::Project, "/tmp"),
                workspace("w2", "dir: tmp", WorkspaceKind::Dir, "/tmp"),
            ],
        );

        assert_eq!(app.workspace_to_close(&project), Some("w1".into()));
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
                entry(Source::Project, "/tmp/project", "project"),
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
            jump_back: JumpBackConfig {
                enabled: true,
                pin_previous: false,
            },
            ..Config::default()
        };
        save_previous_workspace("w-previous").unwrap();

        assert_eq!(jump_back(&config).unwrap(), "Previous");

        assert_eq!(
            RecentState::load().recent_ids(Some("work")),
            &["w-previous"]
        );
        assert_eq!(read_previous_workspace().unwrap(), "w-current");
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
            jump_back: JumpBackConfig {
                enabled: true,
                pin_previous: false,
            },
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

        let mut agent = entry(Source::Agent, "/tmp", "agent");
        agent.workspace_id = Some("w3".into());
        agent.action = EntryAction::FocusAgent {
            target: "pane-1".into(),
        };
        assert_eq!(
            recent_destination_for_entry(&agent).map(|(_, workspace_id)| workspace_id),
            Some("w3".into())
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
        assert!(tracks_workspace_transition(&pane.action));
    }

    #[test]
    fn indirect_success_uses_resulting_workspace_and_failure_has_no_destination() {
        let action = EntryAction::OpenProject;
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
    fn jump_back_records_only_real_workspace_transitions() {
        assert_eq!(
            previous_workspace_to_record(Some("w1"), Some("w2")),
            Some("w1")
        );
        assert_eq!(previous_workspace_to_record(Some("w1"), Some("w1")), None);
        assert_eq!(previous_workspace_to_record(None, Some("w2")), None);
        assert_eq!(previous_workspace_to_record(Some("w1"), None), None);
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
}
