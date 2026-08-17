use std::{path::PathBuf, process::Command};

use crate::{
    config::{Config, SessionEntryConfig},
    herdr::herdr_bin,
    model::{Entry, EntryAction, Source},
};

pub(crate) fn collect_remotes(config: &Config) -> Vec<Entry> {
    config
        .sessions
        .entries
        .iter()
        .filter_map(remote_entry)
        .collect()
}

fn remote_entry(config: &SessionEntryConfig) -> Option<Entry> {
    let target = config.remote.clone()?;
    let mut search_terms = vec!["server".into(), "remote".into(), target.clone()];
    search_terms.extend(config.tags.iter().cloned());
    Some(Entry {
        source: Source::Server,
        title: config.name.clone(),
        subtitle: format!("remote Herdr · {target}"),
        path: PathBuf::from(format!("remote:{target}")),
        workspace_id: None,
        workspace_label: None,
        agent_target: None,
        project: None,
        action: EntryAction::OpenRemote { target },
        source_label: None,
        search_terms,
        open_node: None,
    })
}

pub(crate) fn open_remote(target: &str) -> Result<(), String> {
    let status = Command::new(herdr_bin())
        .args(["--remote", target, "--handoff"])
        .status()
        .map_err(|err| err.to_string())?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("herdr exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_session_entries_are_ignored() {
        let mut config = Config::default();
        config.sessions.entries.push(SessionEntryConfig {
            name: "work".into(),
            remote: None,
            tags: vec![],
        });

        assert!(collect_remotes(&config).is_empty());
    }

    #[test]
    fn manual_remote_entry_opens_remote_target() {
        let entry = remote_entry(&SessionEntryConfig {
            name: "prod".into(),
            remote: Some("prod-box".into()),
            tags: vec!["api".into()],
        })
        .unwrap();

        assert_eq!(entry.source, Source::Server);
        assert!(entry.haystack().contains("prod-box"));
        assert!(matches!(
            entry.action,
            EntryAction::OpenRemote { ref target } if target == "prod-box"
        ));
    }
}
