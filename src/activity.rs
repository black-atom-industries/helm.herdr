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
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct WorkspaceActivity {
    pub(crate) focused_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct PaneActivity {
    pub(crate) status: String,
    pub(crate) since: i64,
}

impl Default for ActivityState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            workspaces: BTreeMap::new(),
            panes: BTreeMap::new(),
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
        change(&mut state);
        state.save_to(path)?;
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

    pub(crate) fn record_agent_status(
        &mut self,
        session: Option<&str>,
        pane_id: &str,
        status: &str,
        now: i64,
    ) {
        let key = workspace_key(session, pane_id);
        if self
            .panes
            .get(&key)
            .is_some_and(|pane| pane.status == status)
        {
            return;
        }
        self.panes.insert(
            key,
            PaneActivity {
                status: status.to_string(),
                since: now,
            },
        );
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
            })
        );
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
