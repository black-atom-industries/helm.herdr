use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::paths::activity_state_path;

const STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ActivityState {
    version: u32,
    #[serde(default)]
    pub(crate) workspaces: BTreeMap<String, WorkspaceActivity>,
    #[serde(default)]
    pub(crate) panes: BTreeMap<String, PaneActivity>,
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

    pub(crate) fn load_from(path: &Path) -> io::Result<Self> {
        let state: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if state.version != STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported activity state version {}", state.version),
            ));
        }
        Ok(state)
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        self.save_to(&activity_state_path())
    }

    pub(crate) fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = serde_json::to_vec_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        fs::write(&temp, encoded)?;
        fs::rename(&temp, path)
    }

    pub(crate) fn record_focus(&mut self, workspace_id: &str, now: i64) {
        self.workspaces.insert(
            workspace_id.to_string(),
            WorkspaceActivity { focused_at: now },
        );
    }

    pub(crate) fn record_agent_status(&mut self, pane_id: &str, status: &str, now: i64) {
        if self
            .panes
            .get(pane_id)
            .is_some_and(|pane| pane.status == status)
        {
            return;
        }
        self.panes.insert(
            pane_id.to_string(),
            PaneActivity {
                status: status.to_string(),
                since: now,
            },
        );
    }

    pub(crate) fn retain(&mut self, workspace_ids: &[String], pane_ids: &[String]) {
        let workspaces: HashSet<&str> = workspace_ids.iter().map(String::as_str).collect();
        let panes: HashSet<&str> = pane_ids.iter().map(String::as_str).collect();
        self.workspaces
            .retain(|id, _| workspaces.contains(id.as_str()));
        self.panes.retain(|id, _| panes.contains(id.as_str()));
    }
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
        std::env::temp_dir().join(format!("helm-activity-{label}-{suffix}.json"))
    }

    #[test]
    fn round_trip_preserves_focus_and_agent_status() {
        let path = temp_path("round-trip");
        let mut state = ActivityState::default();
        state.record_focus("w1", 100);
        state.record_agent_status("w1:p1", "working", 120);

        state.save_to(&path).unwrap();
        assert_eq!(ActivityState::load_from(&path).unwrap(), state);
        let persisted =
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["version"], 1);
        assert_eq!(persisted["workspaces"]["w1"]["focused_at"], 100);
        assert_eq!(persisted["panes"]["w1:p1"]["status"], "working");
        assert_eq!(persisted["panes"]["w1:p1"]["since"], 120);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn agent_status_updates_since_only_when_status_changes() {
        let mut state = ActivityState::default();

        state.record_agent_status("w1:p1", "working", 10);
        state.record_agent_status("w1:p1", "working", 20);
        assert_eq!(state.panes["w1:p1"].since, 10);

        state.record_agent_status("w1:p1", "idle", 30);
        assert_eq!(
            state.panes["w1:p1"],
            PaneActivity {
                status: "idle".into(),
                since: 30,
            }
        );
    }

    #[test]
    fn retain_prunes_workspaces_and_panes_missing_from_snapshot() {
        let mut state = ActivityState::default();
        state.record_focus("w1", 1);
        state.record_focus("w2", 2);
        state.record_agent_status("w1:p1", "idle", 1);
        state.record_agent_status("w2:p1", "done", 2);

        state.retain(&["w1".into()], &["w1:p1".into()]);

        assert_eq!(state.workspaces.keys().collect::<Vec<_>>(), ["w1"]);
        assert_eq!(state.panes.keys().collect::<Vec<_>>(), ["w1:p1"]);
    }

    #[test]
    fn load_from_rejects_missing_and_unsupported_state() {
        let missing = temp_path("missing");
        assert_eq!(
            ActivityState::load_from(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        let path = temp_path("version");
        fs::write(&path, br#"{"version":2,"workspaces":{}}"#).unwrap();
        assert!(ActivityState::load_from(&path).is_err());
        let _ = fs::remove_file(path);
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
