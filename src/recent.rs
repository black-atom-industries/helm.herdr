use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::paths::recent_workspaces_state_path;

const STATE_VERSION: u32 = 1;
pub(crate) const DEFAULT_SESSION_KEY: &str = "d";
const NAMED_SESSION_PREFIX: &str = "n:";
const LEGACY_DEFAULT_SESSION_KEYS: [&str; 2] = ["default", "__default__"];

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
        let mut state: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if state.version != STATE_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported recent state version {}", state.version),
            ));
        }
        state.migrate_legacy_session_keys();
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
        let key = session_key(session);
        self.sessions.get(&key).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn record(&mut self, session: Option<&str>, workspace_id: &str) {
        if workspace_id.is_empty() {
            return;
        }
        let history = self.sessions.entry(session_key(session)).or_default();
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
            .insert(session_key(session), normalized.clone());
        normalized
    }

    fn migrate_legacy_session_keys(&mut self) {
        if self.sessions.keys().all(|key| is_encoded_session_key(key)) {
            return;
        }

        let legacy = std::mem::take(&mut self.sessions);
        let mut migrated = BTreeMap::new();
        for (key, history) in legacy {
            let encoded_key = if LEGACY_DEFAULT_SESSION_KEYS.contains(&key.as_str()) {
                DEFAULT_SESSION_KEY.to_string()
            } else {
                named_session_key(&key)
            };
            migrated
                .entry(encoded_key)
                .or_insert_with(Vec::new)
                .extend(history);
        }
        self.sessions = migrated;
    }
}

pub(crate) fn session_key(session: Option<&str>) -> String {
    session
        .filter(|name| !name.is_empty())
        .map(named_session_key)
        .unwrap_or_else(|| DEFAULT_SESSION_KEY.to_string())
}

fn named_session_key(name: &str) -> String {
    let mut encoded = String::with_capacity(NAMED_SESSION_PREFIX.len() + name.len() * 2);
    encoded.push_str(NAMED_SESSION_PREFIX);
    for byte in name.as_bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn is_encoded_session_key(key: &str) -> bool {
    key == DEFAULT_SESSION_KEY
        || key
            .strip_prefix(NAMED_SESSION_PREFIX)
            .is_some_and(|encoded| {
                !encoded.is_empty()
                    && encoded.len() % 2 == 0
                    && encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
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
    fn encoded_default_key_does_not_collide_with_named_sentinel() {
        let mut state = RecentState::default();
        state.record(None, "default-workspace");
        state.record(Some("__default__"), "named-sentinel-workspace");

        assert_eq!(state.recent_ids(None), &["default-workspace"]);
        assert_eq!(
            state.recent_ids(Some("__default__")),
            &["named-sentinel-workspace"]
        );
        assert_eq!(session_key(None), DEFAULT_SESSION_KEY);
        assert_ne!(session_key(None), session_key(Some("__default__")));
        assert_eq!(state.sessions.len(), 2);
    }

    #[test]
    fn load_migrates_legacy_default_key() {
        let path = temp_path("legacy-default");
        fs::write(
            &path,
            br#"{"version":1,"sessions":{"default":["old-workspace"]}}"#,
        )
        .unwrap();

        let state = RecentState::load_from(&path).unwrap();

        assert_eq!(state.recent_ids(None), &["old-workspace"]);
        assert!(!state.sessions.contains_key("default"));
        assert!(state.sessions.contains_key(DEFAULT_SESSION_KEY));
        let _ = fs::remove_file(path);
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
