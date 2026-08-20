use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::paths::recent_workspaces_state_path;

const STATE_VERSION: u32 = 1;
pub(crate) const DEFAULT_SESSION_KEY: &str = "__default__";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct RecentState {
    version: u32,
    pub(crate) sessions: BTreeMap<String, Vec<String>>,
}

impl Default for RecentState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            sessions: BTreeMap::new(),
        }
    }
}

impl RecentState {
    pub(crate) fn load() -> Self {
        Self::load_from(&recent_workspaces_state_path()).unwrap_or_default()
    }

    pub(crate) fn load_from(path: &Path) -> io::Result<Self> {
        let state: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if state.version != STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported recent state version {}", state.version),
            ));
        }
        Ok(state)
    }

    pub(crate) fn save(&self) {
        let _ = self.save_to(&recent_workspaces_state_path());
    }

    pub(crate) fn save_to(&self, path: &Path) -> io::Result<()> {
        if self.version != STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported recent state version {}", self.version),
            ));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let encoded = serde_json::to_vec_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(path, encoded)
    }

    pub(crate) fn recent_ids(&self, session: Option<&str>) -> &[String] {
        self.sessions
            .get(session_key(session))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn record(&mut self, session: Option<&str>, workspace_id: &str) {
        if workspace_id.is_empty() {
            return;
        }
        let history = self
            .sessions
            .entry(session_key(session).to_string())
            .or_default();
        history.retain(|id| id != workspace_id);
        history.insert(0, workspace_id.to_string());
    }

    pub(crate) fn normalize(
        &mut self,
        session: Option<&str>,
        live_ids: &[String],
        current_id: Option<&str>,
    ) -> Vec<String> {
        let live: HashSet<&str> = live_ids.iter().map(String::as_str).collect();
        let mut seen = HashSet::new();
        let mut normalized = Vec::with_capacity(live_ids.len());

        if let Some(current) = current_id.filter(|id| live.contains(id)) {
            normalized.push(current.to_string());
            seen.insert(current);
        }
        for id in self.recent_ids(session) {
            if live.contains(id.as_str()) && seen.insert(id.as_str()) {
                normalized.push(id.clone());
            }
        }
        for id in live_ids {
            if seen.insert(id.as_str()) {
                normalized.push(id.clone());
            }
        }

        self.sessions
            .insert(session_key(session).to_string(), normalized.clone());
        normalized
    }
}

pub(crate) fn session_key(session: Option<&str>) -> &str {
    session
        .filter(|name| !name.is_empty())
        .unwrap_or(DEFAULT_SESSION_KEY)
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
        std::env::temp_dir().join(format!("helm-recent-{label}-{suffix}.json"))
    }

    #[test]
    fn round_trip_preserves_versioned_session_state() {
        let path = temp_path("round-trip");
        let mut state = RecentState::default();
        state.record(None, "default-workspace");
        state.record(Some("work"), "named-workspace");

        state.save_to(&path).unwrap();
        assert_eq!(RecentState::load_from(&path).unwrap(), state);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap()
                ["version"],
            1
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn default_session_sentinel_does_not_collide_with_named_default() {
        let mut state = RecentState::default();
        state.record(None, "default-workspace");
        state.record(Some("default"), "named-default-workspace");

        assert_eq!(state.recent_ids(None), &["default-workspace"]);
        assert_eq!(
            state.recent_ids(Some("default")),
            &["named-default-workspace"]
        );
        assert_eq!(session_key(None), DEFAULT_SESSION_KEY);
        assert_ne!(session_key(None), session_key(Some("default")));
    }

    #[test]
    fn record_moves_to_front_and_removes_duplicates() {
        let mut state = RecentState::default();
        state.sessions.insert(
            DEFAULT_SESSION_KEY.into(),
            vec!["a".into(), "b".into(), "a".into()],
        );

        state.record(None, "a");

        assert_eq!(state.recent_ids(None), &["a", "b"]);
    }

    #[test]
    fn normalize_prunes_stale_and_duplicate_ids_and_appends_live_api_order() {
        let mut state = RecentState::default();
        state.sessions.insert(
            DEFAULT_SESSION_KEY.into(),
            vec!["stale".into(), "b".into(), "b".into(), "a".into()],
        );
        let live = vec!["a".into(), "b".into(), "c".into()];

        let normalized = state.normalize(None, &live, Some("b"));

        assert_eq!(normalized, vec!["b", "a", "c"]);
        assert_eq!(state.recent_ids(None), normalized.as_slice());
    }

    #[test]
    fn normalize_keeps_current_first_even_when_it_was_not_recorded() {
        let mut state = RecentState::default();
        state.record(Some("work"), "a");
        let live = vec!["a".into(), "b".into()];

        assert_eq!(
            state.normalize(Some("work"), &live, Some("b")),
            vec!["b", "a"]
        );
    }

    #[test]
    fn missing_malformed_and_unreadable_state_fall_back_to_empty() {
        let missing = temp_path("missing");
        assert_eq!(
            RecentState::load_from(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            RecentState::load_from(&missing).unwrap_or_default(),
            RecentState::default()
        );

        let malformed = temp_path("malformed");
        fs::write(&malformed, b"not json").unwrap();
        assert!(RecentState::load_from(&malformed).is_err());
        assert_eq!(
            RecentState::load_from(&malformed).unwrap_or_default(),
            RecentState::default()
        );

        let unreadable = temp_path("unreadable");
        fs::create_dir(&unreadable).unwrap();
        assert!(RecentState::load_from(&unreadable).is_err());
        assert_eq!(
            RecentState::load_from(&unreadable).unwrap_or_default(),
            RecentState::default()
        );

        let _ = fs::remove_file(malformed);
        let _ = fs::remove_dir(unreadable);
    }

    #[test]
    fn unsupported_versions_fall_back_safely() {
        let path = temp_path("version");
        fs::write(
            &path,
            br#"{"version":2,"sessions":{"default":["workspace"]}}"#,
        )
        .unwrap();

        assert!(RecentState::load_from(&path).is_err());
        assert_eq!(
            RecentState::load_from(&path).unwrap_or_default(),
            RecentState::default()
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn persistence_failures_are_reported_without_panicking() {
        let path = temp_path("parent-file");
        fs::write(&path, b"occupied").unwrap();
        let target = path.join("recent-workspaces.json");

        assert!(RecentState::default().save_to(&target).is_err());
        let _ = fs::remove_file(path);
    }
}
