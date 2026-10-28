use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{model::workspace_key, paths::activity_state_path, topology::OpenTopology};

const STATE_VERSION: u32 = 1;

/// Activity of the Workspaces and Panes of every Herdr session. Keys are
/// `workspace_key(session, id)`, because Herdr ids repeat across sessions.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ActivityState {
    version: u32,
    #[serde(default)]
    workspaces: BTreeMap<String, WorkspaceActivity>,
    #[serde(default)]
    panes: BTreeMap<String, PaneActivity>,
    /// Keyed by session, `<default>` for the default one.
    #[serde(default)]
    sessions: BTreeMap<String, SessionActivity>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct WorkspaceActivity {
    pub(crate) focused_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SessionActivity {
    /// When the User was last in the session.
    pub(crate) seen_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct PaneActivity {
    pub(crate) status: String,
    pub(crate) since: i64,
    /// When the agent last finished. Herdr turns `done` into `idle` once the
    /// pane is seen; this stays through that idle and clears when it works or
    /// blocks again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) done_at: Option<i64>,
    /// When the User dismissed the pane's done state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dismissed_at: Option<i64>,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            workspaces: BTreeMap::new(),
            panes: BTreeMap::new(),
            sessions: BTreeMap::new(),
        }
    }
}

impl ActivityState {
    pub(crate) fn load() -> Self {
        Self::load_from(&activity_state_path()).unwrap_or_default()
    }

    /// Reads the state and drops keys that carry no session.
    pub(crate) fn load_from(path: &Path) -> io::Result<Self> {
        let mut state: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if state.version != STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported activity state version {}", state.version),
            ));
        }
        state.workspaces.retain(|key, _| split_key(key).is_some());
        state.panes.retain(|key, _| split_key(key).is_some());
        Ok(state)
    }

    /// Applies `change` to the stored state and saves it, holding an exclusive
    /// lock so concurrent hooks and Pickers never drop each other's writes.
    pub(crate) fn update(change: impl FnOnce(&mut Self)) -> io::Result<Self> {
        Self::update_at(&activity_state_path(), change)
    }

    pub(crate) fn update_at(path: &Path, change: impl FnOnce(&mut Self)) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_extension("json.lock"))?;
        lock.lock()?;
        let mut state = Self::load_from(path).unwrap_or_default();
        let before = state.clone();
        change(&mut state);
        state.save_to(path)?;
        log_changes(&path.with_file_name("debug.log"), &before, &state);
        Ok(state)
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        let encoded = serde_json::to_vec_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        fs::write(&temp, encoded)?;
        fs::rename(&temp, path)
    }

    pub(crate) fn focused_at(&self, session: Option<&str>, workspace_id: &str) -> Option<i64> {
        self.workspaces
            .get(&workspace_key(session, workspace_id))
            .map(|recorded| recorded.focused_at)
    }

    pub(crate) fn pane(&self, session: Option<&str>, pane_id: &str) -> Option<&PaneActivity> {
        self.panes.get(&workspace_key(session, pane_id))
    }

    pub(crate) fn record_focus(&mut self, session: Option<&str>, workspace_id: &str, now: i64) {
        self.workspaces.insert(
            workspace_key(session, workspace_id),
            WorkspaceActivity { focused_at: now },
        );
    }

    /// When the User was last in `session`: its recorded seen time, else the
    /// newest focus of its Workspaces.
    pub(crate) fn seen_at(&self, session: Option<&str>) -> Option<i64> {
        if let Some(recorded) = self.sessions.get(&session_key(session)) {
            return Some(recorded.seen_at);
        }
        let owner = workspace_key(named(session), "");
        self.workspaces
            .iter()
            .filter(|(key, _)| split_key(key).is_some_and(|(prefix, _)| prefix == owner))
            .map(|(_, recorded)| recorded.focused_at)
            .max()
    }

    /// Records that the User came back to `session`. Herdr kept that
    /// session's focus, so an agent that went idle while the User was away
    /// finished unseen and keeps its done state.
    pub(crate) fn record_return(&mut self, session: Option<&str>, now: i64) {
        if let Some(seen) = self.seen_at(session) {
            let owner = workspace_key(named(session), "");
            for (key, pane) in &mut self.panes {
                if split_key(key).is_some_and(|(prefix, _)| prefix == owner)
                    && pane.status == "idle"
                    && pane.since > seen
                    && pane.done_at.is_none()
                {
                    pane.done_at = Some(pane.since);
                }
            }
        }
        self.record_seen(session, now);
    }

    pub(crate) fn record_seen(&mut self, session: Option<&str>, now: i64) {
        self.sessions
            .insert(session_key(session), SessionActivity { seen_at: now });
    }

    pub(crate) fn record_agent_status(
        &mut self,
        session: Option<&str>,
        pane_id: &str,
        status: &str,
        now: i64,
    ) {
        let key = workspace_key(session, pane_id);
        let previous = self.panes.get(&key);
        if previous.is_some_and(|pane| pane.status == status) {
            return;
        }
        let done_at = match status {
            "done" => Some(now),
            "idle" => previous.and_then(|pane| pane.done_at),
            _ => None,
        };
        let dismissed_at = previous.and_then(|pane| pane.dismissed_at);
        self.panes.insert(
            key,
            PaneActivity {
                status: status.to_string(),
                since: now,
                done_at,
                dismissed_at,
            },
        );
    }

    /// Marks the pane's current done state as handled. `status` is its live
    /// status, recorded when the pane has no record yet.
    pub(crate) fn dismiss(&mut self, session: Option<&str>, pane_id: &str, status: &str, now: i64) {
        self.panes
            .entry(workspace_key(session, pane_id))
            .or_insert_with(|| PaneActivity {
                status: status.to_string(),
                since: now,
                done_at: None,
                dismissed_at: None,
            })
            .dismissed_at = Some(now);
    }

    /// Drops the Workspaces and Panes of the snapshot's session that it no
    /// longer lists. A snapshot without Panes keeps every Pane.
    pub(crate) fn retain_topology(&mut self, topology: &OpenTopology) {
        let Some(first) = topology.workspaces.first() else {
            return;
        };
        let workspaces: Vec<String> = topology
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect();
        let panes: Vec<String> = topology
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .map(|pane| pane.id.clone())
            .collect();
        let panes = (!panes.is_empty()).then_some(panes.as_slice());
        self.retain_session(first.session.as_deref(), &workspaces, panes);
    }

    /// Drops the Workspaces and Panes of `session` missing from its live
    /// snapshot. Other sessions are untouched, and Panes are kept when
    /// `pane_ids` is `None`.
    pub(crate) fn retain_session(
        &mut self,
        session: Option<&str>,
        workspace_ids: &[String],
        pane_ids: Option<&[String]>,
    ) {
        let owner = workspace_key(session, "");
        let keep = |ids: &[String]| -> HashSet<String> {
            ids.iter().map(|id| workspace_key(session, id)).collect()
        };
        let live = keep(workspace_ids);
        self.workspaces.retain(|key, _| {
            split_key(key).is_some_and(|(prefix, _)| prefix != owner) || live.contains(key)
        });
        if let Some(pane_ids) = pane_ids {
            let live = keep(pane_ids);
            self.panes.retain(|key, _| {
                split_key(key).is_some_and(|(prefix, _)| prefix != owner) || live.contains(key)
            });
        }
    }
}

/// The `sessions` key of `session`; `None` and `default` name the default session.
fn session_key(session: Option<&str>) -> String {
    workspace_key(named(session), "")
        .trim_end_matches("::")
        .to_string()
}

fn named(session: Option<&str>) -> Option<&str> {
    session.filter(|name| *name != "default")
}

/// Splits a key into its `session::` prefix and the Herdr id. Herdr ids never
/// contain `::`, so the last one separates them.
fn split_key(key: &str) -> Option<(&str, &str)> {
    key.rfind("::").map(|at| key.split_at(at + 2))
}

pub(crate) fn parse_agent_status_event(event: &str) -> Result<(String, String), String> {
    let event = serde_json::from_str::<serde_json::Value>(event)
        .map_err(|e| format!("failed to parse agent status event: {e}"))?;
    let pane_id = event
        .pointer("/data/pane_id")
        .and_then(|v| v.as_str())
        .ok_or("agent status event has no pane_id")?;
    let status = event
        .pointer("/data/agent_status")
        .and_then(|v| v.as_str())
        .ok_or("agent status event has no agent_status")?;
    Ok((pane_id.into(), status.into()))
}

/// Appends one line per changed entry to `log`, only when that file exists.
fn log_changes(log: &Path, before: &ActivityState, after: &ActivityState) {
    let Ok(mut file) = fs::OpenOptions::new().append(true).open(log) else {
        return;
    };
    let prefix = format!(
        "{} {}",
        now_unix_secs(),
        std::env::args().nth(1).unwrap_or_else(|| "ui".into())
    );
    let mut lines = String::new();
    changes(
        &mut lines,
        &prefix,
        "workspace",
        &before.workspaces,
        &after.workspaces,
    );
    changes(&mut lines, &prefix, "pane", &before.panes, &after.panes);
    changes(
        &mut lines,
        &prefix,
        "session",
        &before.sessions,
        &after.sessions,
    );
    if !lines.is_empty() {
        let _ = io::Write::write_all(&mut file, lines.as_bytes());
    }
}

fn changes<V: std::fmt::Debug + PartialEq>(
    lines: &mut String,
    prefix: &str,
    kind: &str,
    before: &BTreeMap<String, V>,
    after: &BTreeMap<String, V>,
) {
    let keys: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for key in keys {
        let (old, new) = (before.get(key), after.get(key));
        if old != new {
            let show = |value: Option<&V>| value.map_or("-".into(), |value| format!("{value:?}"));
            lines.push_str(&format!(
                "{prefix} {kind} {key} {} -> {}\n",
                show(old),
                show(new)
            ));
        }
    }
}

pub(crate) fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!(
                "helm-activity-{label}-{}-{suffix}",
                std::process::id()
            ))
            .join("activity.json")
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn round_trip_preserves_focus_and_agent_status() {
        let path = temp_path("round-trip");
        let state = ActivityState::update_at(&path, |state| {
            state.record_focus(Some("work"), "w1", 100);
            state.record_agent_status(Some("work"), "w1:p1", "working", 120);
        })
        .unwrap();

        assert_eq!(ActivityState::load_from(&path).unwrap(), state);
        let persisted =
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["version"], 1);
        assert_eq!(persisted["workspaces"]["work::w1"]["focused_at"], 100);
        assert_eq!(persisted["panes"]["work::w1:p1"]["status"], "working");
        assert_eq!(persisted["panes"]["work::w1:p1"]["since"], 120);
        cleanup(&path);
    }

    #[test]
    fn changes_go_to_the_debug_log_only_when_it_exists() {
        let path = temp_path("debug-log");
        let log = path.with_file_name("debug.log");
        ActivityState::update_at(&path, |state| state.record_focus(Some("work"), "w1", 100))
            .unwrap();
        assert!(!log.exists());

        fs::write(&log, "").unwrap();
        ActivityState::update_at(&path, |state| {
            state.record_agent_status(Some("work"), "w1:p1", "done", 120);
        })
        .unwrap();

        let logged = fs::read_to_string(&log).unwrap();
        assert_eq!(logged.lines().count(), 1);
        assert!(logged.contains(" pane work::w1:p1 - -> PaneActivity { status: \"done\""));
        cleanup(&path);
    }

    #[test]
    fn agent_status_updates_since_only_when_status_changes() {
        let mut state = ActivityState::default();

        state.record_agent_status(None, "w1:p1", "working", 10);
        state.record_agent_status(None, "w1:p1", "working", 20);
        assert_eq!(state.pane(None, "w1:p1").unwrap().since, 10);

        state.record_agent_status(None, "w1:p1", "idle", 30);
        assert_eq!(
            state.pane(None, "w1:p1"),
            Some(&PaneActivity {
                status: "idle".into(),
                since: 30,
                done_at: None,
                dismissed_at: None,
            })
        );
    }

    #[test]
    fn a_done_agent_stays_done_through_idle_until_it_works_again() {
        let mut state = ActivityState::default();
        state.record_agent_status(None, "w1:p1", "done", 10);
        state.record_agent_status(None, "w1:p1", "idle", 20);
        assert_eq!(state.pane(None, "w1:p1").unwrap().done_at, Some(10));
        state.record_agent_status(None, "w1:p1", "working", 30);
        assert_eq!(state.pane(None, "w1:p1").unwrap().done_at, None);
    }

    #[test]
    fn returning_keeps_an_agent_that_went_idle_while_away_done() {
        let mut state = ActivityState::default();
        state.record_agent_status(Some("work"), "w1:p1", "idle", 5);
        state.record_seen(Some("work"), 10);
        state.record_agent_status(Some("work"), "w1:p2", "idle", 20);
        state.record_agent_status(Some("play"), "w1:p3", "idle", 20);
        state.record_return(Some("work"), 30);
        assert_eq!(state.pane(Some("work"), "w1:p1").unwrap().done_at, None);
        assert_eq!(state.pane(Some("work"), "w1:p2").unwrap().done_at, Some(20));
        assert_eq!(state.pane(Some("play"), "w1:p3").unwrap().done_at, None);
        assert_eq!(state.seen_at(Some("work")), Some(30));
    }

    #[test]
    fn equal_ids_in_two_sessions_stay_apart() {
        let mut state = ActivityState::default();
        state.record_focus(Some("work"), "w2", 1);
        state.record_focus(Some("play"), "w2", 2);
        state.record_agent_status(Some("work"), "w2:p1", "idle", 1);
        state.record_agent_status(Some("play"), "w2:p1", "done", 2);

        assert_eq!(state.focused_at(Some("work"), "w2"), Some(1));
        assert_eq!(state.focused_at(Some("play"), "w2"), Some(2));
        assert_eq!(state.focused_at(None, "w2"), None);
        assert_eq!(state.pane(Some("play"), "w2:p1").unwrap().status, "done");
    }

    #[test]
    fn pruning_one_session_keeps_the_other_sessions() {
        let mut state = ActivityState::default();
        for session in [Some("work"), Some("play"), None] {
            state.record_focus(session, "w1", 1);
            state.record_focus(session, "w2", 2);
            state.record_agent_status(session, "w1:p1", "idle", 1);
            state.record_agent_status(session, "w2:p1", "done", 2);
        }

        state.retain_session(Some("work"), &["w1".into()], Some(&["w1:p1".into()]));

        assert_eq!(state.focused_at(Some("work"), "w1"), Some(1));
        assert_eq!(state.focused_at(Some("work"), "w2"), None);
        assert!(state.pane(Some("work"), "w2:p1").is_none());
        for session in [Some("play"), None] {
            assert_eq!(state.focused_at(session, "w2"), Some(2));
            assert!(state.pane(session, "w2:p1").is_some());
        }
    }

    fn snapshot(session: &str, ids: &[&str], panes: bool) -> OpenTopology {
        use crate::topology::{PaneNode, TabNode, WorkspaceNode};
        OpenTopology {
            workspaces: ids
                .iter()
                .map(|id| WorkspaceNode {
                    id: (*id).into(),
                    label: (*id).into(),
                    session: Some(session.into()),
                    focused: false,
                    tabs: vec![TabNode {
                        id: format!("{id}:t1"),
                        label: "code".into(),
                        focused: true,
                        panes: if panes {
                            vec![PaneNode {
                                id: format!("{id}:p1"),
                                label: "p1".into(),
                                cwd: Default::default(),
                                focused: false,
                                title: None,
                                agent_session: None,
                                agent: None,
                            }]
                        } else {
                            Vec::new()
                        },
                    }],
                    git: None,
                })
                .collect(),
        }
    }

    fn two_sessions() -> ActivityState {
        let mut state = ActivityState::default();
        for session in [Some("work"), Some("play")] {
            for id in ["w1", "w2"] {
                state.record_focus(session, id, 1);
                state.record_agent_status(session, &format!("{id}:p1"), "idle", 1);
            }
        }
        state
    }

    #[test]
    fn a_snapshot_prunes_only_its_own_session() {
        let mut state = two_sessions();
        state.retain_topology(&snapshot("work", &["w1"], true));

        assert!(state.focused_at(Some("work"), "w2").is_none());
        assert!(state.pane(Some("work"), "w2:p1").is_none());
        assert!(state.pane(Some("work"), "w1:p1").is_some());
        assert!(state.focused_at(Some("play"), "w2").is_some());
        assert!(state.pane(Some("play"), "w2:p1").is_some());
    }

    #[test]
    fn a_snapshot_without_panes_keeps_every_pane() {
        let mut state = two_sessions();
        state.retain_topology(&snapshot("work", &["w1"], false));

        assert!(state.focused_at(Some("work"), "w2").is_none());
        assert!(state.pane(Some("work"), "w2:p1").is_some());
    }

    #[test]
    fn an_empty_snapshot_prunes_nothing() {
        let mut state = two_sessions();
        state.retain_topology(&OpenTopology::default());
        assert_eq!(state, two_sessions());
    }

    #[test]
    fn load_drops_keys_without_a_session() {
        let path = temp_path("legacy");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            br#"{"version":1,"workspaces":{"w1":{"focused_at":1},"work::w1":{"focused_at":2}},"panes":{"w1:p1":{"status":"idle","since":1}}}"#,
        )
        .unwrap();

        let state = ActivityState::load_from(&path).unwrap();
        assert_eq!(state.workspaces.keys().collect::<Vec<_>>(), ["work::w1"]);
        assert!(state.panes.is_empty());
        cleanup(&path);
    }

    #[test]
    fn seen_time_falls_back_to_the_newest_focus_of_the_session() {
        let path = temp_path("seen");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            br#"{"version":1,"workspaces":{"work::w1":{"focused_at":5},"work::w2":{"focused_at":9},"<default>::w1":{"focused_at":20}}}"#,
        )
        .unwrap();
        let mut state = ActivityState::load_from(&path).unwrap();
        assert_eq!(state.seen_at(Some("work")), Some(9));
        assert_eq!(state.seen_at(Some("default")), Some(20));
        assert_eq!(state.seen_at(Some("play")), None);

        state.record_seen(Some("work"), 3);
        state.record_seen(Some("default"), 30);
        assert_eq!(state.seen_at(Some("work")), Some(3));
        assert_eq!(state.seen_at(None), Some(30));
        state.save_to(&path).unwrap();
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["sessions"]["work"]["seen_at"], 3);
        assert_eq!(persisted["sessions"]["<default>"]["seen_at"], 30);
        cleanup(&path);
    }

    #[test]
    fn concurrent_updates_keep_every_write() {
        let path = temp_path("concurrent");
        let writers: Vec<_> = (0..16)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || {
                    ActivityState::update_at(&path, |state| {
                        state.record_focus(None, &format!("w{index}"), index);
                    })
                    .unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        let state = ActivityState::load_from(&path).unwrap();
        assert_eq!(state.workspaces.len(), 16);
        cleanup(&path);
    }

    #[test]
    fn load_from_rejects_missing_and_unsupported_state() {
        let missing = temp_path("missing");
        assert_eq!(
            ActivityState::load_from(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        let path = temp_path("version");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, br#"{"version":2,"workspaces":{}}"#).unwrap();
        assert!(ActivityState::load_from(&path).is_err());
        cleanup(&path);
    }

    #[test]
    fn parses_agent_status_payload() {
        let event = r#"{"event":"pane.agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p1","workspace_id":"w1","agent_status":"blocked"}}"#;

        assert_eq!(
            parse_agent_status_event(event).unwrap(),
            ("w1:p1".into(), "blocked".into())
        );
    }

    #[test]
    fn rejects_agent_status_payload_without_pane_or_status() {
        assert!(parse_agent_status_event("not json").is_err());
        assert!(parse_agent_status_event(r#"{"data":{"pane_id":"w1:p1"}}"#).is_err());
        assert!(parse_agent_status_event(r#"{"data":{"agent_status":"idle"}}"#).is_err());
    }
}
