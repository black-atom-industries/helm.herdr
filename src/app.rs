use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
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
    sources::{collect_open_topology, collect_roots, collect_zoxide},
    theme::Theme,
    topology::{self, OpenTopology},
};

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
    pub(crate) recent_state: RecentState,
    pub(crate) spinner_tick: u32,
    pub(crate) update_available: Option<String>,
    pub(crate) topology: OpenTopology,
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
    /// Largest useful detail offset, set by the last render.
    pub(crate) detail_max_offset: usize,
    /// Running Claude subagents, keyed by pane id.
    pub(crate) subagents: HashMap<String, Vec<Subagent>>,
    /// Git reads of the glance workspaces, keyed by workspace key.
    pub(crate) git_detail: HashMap<String, CachedGit>,
    #[cfg(test)]
    pub(crate) herdr_control: Option<Box<dyn HerdrControl>>,
    initial_selection_pending: bool,
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
            recent_state: RecentState::default(),
            spinner_tick: 0,
            update_available: None,
            topology: OpenTopology::default(),
            topology_entries: Vec::new(),
            activity: ActivityState::default(),
            glance: HashMap::new(),
            row_ranks: HashMap::new(),
            now: 0,
            detail_scroll: DetailScroll::default(),
            last_label: None,
            close_armed: false,
            detail_max_offset: 0,
            subagents: HashMap::new(),
            git_detail: HashMap::new(),
            #[cfg(test)]
            herdr_control: None,
            initial_selection_pending: true,
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
        if let Some(activity) = prune_activity(&workspaces.topology) {
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
        self.pinned_entries =
            read_pinned_entries(&plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE))
                .unwrap_or_default();
        self.set_workspace_state(workspaces, now_unix_secs());
        self.row_ranks.clear();
        if migrate_legacy_topology_pins(&self.entries, &mut self.pinned_entries) {
            let _ = save_pinned_entries(
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
        if workspaces.topology.workspaces.is_empty() && !self.topology.workspaces.is_empty() {
            return;
        }
        let selected = self.selected_entry().map(entry_identity);
        self.set_workspace_state(workspaces, now);
        self.rebuild_glance();
        self.apply_filter();
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
        self.activity = workspaces.activity;
        self.previous_workspace_id = workspaces.previous;
        self.subagents = workspaces.subagents;
        self.now = now;
        self.topology_entries = topology::query_entries(&self.topology);
        self.entries
            .retain(|entry| entry.source != Source::Workspace);
        self.entries.extend(self.topology_entries.iter().cloned());
    }

    pub(crate) fn rebuild_glance(&mut self) {
        let thresholds = Thresholds {
            active_secs: self.config.glance.active_minutes * 60,
            recent_secs: self.config.glance.recent_hours * 3_600,
        };
        self.glance = self
            .topology
            .workspaces
            .iter()
            .map(|workspace| {
                let pinned = self
                    .topology_entries
                    .iter()
                    .find(|entry| {
                        matches!(&entry.action, EntryAction::FocusWorkspace { session, id } if id == &workspace.id && session == &workspace.session)
                    })
                    .is_some_and(|entry| self.is_pinned(entry));
                let evidence = glance::evidence(workspace, &self.activity, pinned);
                let previous = self.previous_workspace_id.as_deref() == Some(workspace.id.as_str());
                let row = glance::row(workspace, &evidence, previous, self.now, thresholds);
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

    pub(crate) fn apply_filter(&mut self) {
        let query = Query::parse(&self.query);
        let searching = !self.query.trim().is_empty();
        let glance = self.glance_view();

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
            }
            if !query.filters_match(entry) {
                continue;
            }
            let bonus = self.config.picker.source_bonus(&entry.source);
            if query.plain.is_empty() {
                scored.push((bonus, idx));
            } else if let Some(score) =
                match_score(&self.config.picker.engine, &entry.haystack(), &query.plain)
            {
                scored.push((score + bonus, idx));
            }
        }
        if glance {
            scored.sort_by_key(|(score, idx)| self.glance_order(&self.entries[*idx], *score));
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
        self.selected = if searching && !glance {
            self.best_search_position(&query).unwrap_or(0)
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
                    match_score(&self.config.picker.engine, &entry.haystack(), &query.plain)?
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

    fn glance_row_at(&self, position: usize) -> Option<&GlanceRow> {
        let index = *self.filtered.get(position)?;
        self.glance_row(&self.entries[index])
    }

    fn group_at(&self, position: usize) -> Option<Group> {
        let index = *self.filtered.get(position)?;
        self.glance_group(&self.entries[index])
    }

    /// Positions of the labelled rows, in label order: the first visible
    /// glance rows, Pinned first.
    pub(crate) fn labelled_positions(&self) -> Vec<usize> {
        if !self.glance_view() {
            return Vec::new();
        }
        let count = self.config.glance.labels().len();
        (0..self.filtered.len())
            .filter(|&position| self.glance_row_at(position).is_some())
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

    /// The row marked `last`, which is the Previous workspace, when it is listed.
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
            .filter_map(|pane| self.subagents.get(&pane.id))
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
        if !(removed_primary || removed_legacy) {
            pinned.insert(key);
        }
        save_pinned_entries(
            &plugin_config_dir().join(PINNED_ENTRIES_STATE_FILE),
            &pinned,
        )?;
        self.pinned_entries = pinned;
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

    pub(crate) fn open_selected(&mut self, use_directory_template: bool) -> Result<(), String> {
        let e = self.selected_entry().cloned().ok_or("nothing selected")?;
        self.open_entry(e, use_directory_template)
    }

    /// Opens tab `number` (1-based) of the selected Workspace.
    pub(crate) fn open_tab(&mut self, number: usize) -> Result<(), String> {
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
            Source::Zoxide | Source::Root => self.matching_dir_workspace(e).map(|ws| ws.id.clone()),
            Source::QuickAction | Source::Integration => None,
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

fn current_session_name() -> Option<String> {
    env::var("HERDR_SESSION")
        .ok()
        .filter(|name| !name.trim().is_empty())
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
    let session = current_session_name();
    update_activity(|activity| activity.record_focus(session.as_deref(), focused, now_unix_secs()))
}

pub(crate) fn track_agent() -> Result<(), String> {
    let event = env::var("HERDR_PLUGIN_EVENT_JSON")
        .map_err(|_| "HERDR_PLUGIN_EVENT_JSON is not set".to_string())?;
    let (pane_id, status) = parse_agent_status_event(&event)?;
    let now = now_unix_secs();
    let session = current_session_name();
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
}

/// Reads the Herdr snapshot and activity. A `snapshot` already read from Herdr
/// is parsed instead of asking again. Git identities come from `cached` when
/// given, and from git itself otherwise.
pub(crate) fn load_workspaces(
    config: &Config,
    cached: Option<&OpenTopology>,
    snapshot: Option<&serde_json::Value>,
) -> Workspaces {
    let topology = if config.sources.open_workspaces {
        let session = current_session_name();
        let mut topology = snapshot
            .map(|snapshot| topology::parse_snapshot(snapshot, session.clone()))
            .filter(|topology| !topology.workspaces.is_empty())
            .unwrap_or_else(|| {
                topology::collect_topology(|args| herdr_json_args(args.iter().copied()), session)
            });
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
        activity: ActivityState::load(),
        previous: config
            .jump_back
            .enabled
            .then(|| read_previous_workspace().ok())
            .flatten(),
        subagents,
    }
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
        save_pinned_entries(&path, &app.pinned_entries).unwrap();
        assert_eq!(read_pinned_entries(&path).unwrap(), app.pinned_entries);
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
            Some(Group::Active)
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
            },
            NOW,
        );

        for (query, expected) in [
            ("agent", vec!["helm"]),
            ("!claude", vec!["helm"]),
            ("!codex", vec![]),
            ("#working", vec!["helm"]),
            ("@working", vec!["helm"]),
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
}
