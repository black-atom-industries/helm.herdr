use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths::canonical_str;

/// Identifies a Herdr id within its session. Herdr ids repeat across sessions.
pub(crate) fn workspace_key(session: Option<&str>, id: &str) -> String {
    format!("{}::{id}", session.unwrap_or("<default>"))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    Workspace,
    Session,
    Zoxide,
    Root,
    QuickAction,
    Integration,
}

impl Source {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Source::Workspace => "open",
            Source::Session => "session",
            Source::Zoxide => "zoxide",
            Source::Root => "root",
            Source::QuickAction => "quick",
            Source::Integration => "plugin",
        }
    }

    pub(crate) fn from_config(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "workspace" | "workspaces" | "open" | "open_workspaces" => Some(Source::Workspace),
            "session" | "sessions" => Some(Source::Session),
            "zoxide" | "z" => Some(Source::Zoxide),
            "root" | "roots" | "scan" => Some(Source::Root),
            "quick" | "quick_action" | "quick_actions" | "herdr_plus_quick_actions" => {
                Some(Source::QuickAction)
            }
            "plugin" | "integration" | "integrations" => Some(Source::Integration),
            _ => None,
        }
    }

    pub(crate) fn all() -> [Source; 6] {
        [
            Source::Workspace,
            Source::Session,
            Source::Zoxide,
            Source::Root,
            Source::QuickAction,
            Source::Integration,
        ]
    }
}

#[derive(Clone, Debug)]
pub(crate) enum EntryAction {
    FocusWorkspace {
        session: Option<String>,
        id: String,
    },
    FocusTab {
        session: Option<String>,
        id: String,
    },
    #[allow(dead_code)]
    FocusPane {
        session: Option<String>,
        id: String,
    },
    /// Opens another Herdr session, at one of its Workspaces and Tabs when given.
    SwitchSession {
        session: Option<String>,
        workspace: Option<String>,
        tab: Option<String>,
    },
    InvokePluginAction {
        action: String,
    },
    FocusOrCreateDir,
    RunCommand {
        command: String,
        notify_success: bool,
        notify_error: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceKind {
    Project,
    Dir,
    Unknown,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct WorkspaceRef {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) kind: WorkspaceKind,
    pub(crate) path: PathBuf,
    pub(crate) tab_count: i64,
    pub(crate) pane_count: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) source: Source,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) path: PathBuf,
    pub(crate) workspace_id: Option<String>,
    pub(crate) workspace_label: Option<String>,
    pub(crate) agent_target: Option<String>,
    pub(crate) action: EntryAction,
    pub(crate) source_label: Option<String>,
    pub(crate) search_terms: Vec<String>,
}

impl Entry {
    pub(crate) fn key(&self) -> String {
        if let EntryAction::FocusPane { session, id } = &self.action {
            return format!("pane:{}:{id}", session.as_deref().unwrap_or("<default>"));
        }
        canonical_str(&self.path).unwrap_or_else(|| self.path.display().to_string())
    }

    pub(crate) fn source_name(&self) -> &str {
        self.source_label
            .as_deref()
            .unwrap_or_else(|| self.source.label())
    }

    pub(crate) fn haystack(&self) -> String {
        format!(
            "{} {} {} {} {} {}",
            self.source_name(),
            self.title,
            self.subtitle,
            self.workspace_label.as_deref().unwrap_or(""),
            self.path.display(),
            self.search_terms
                .iter()
                .filter(|term| !is_machine_term(term))
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        )
        .to_lowercase()
    }
}

/// Prefixes of machine-only search terms: agent names and statuses that Query
/// tokens read, and identity keys. They stay out of the fuzzy haystack, where
/// their keys and ids would match loose text.
const MACHINE_TERM_PREFIXES: [&str; 4] = ["agent-name:", "agent-status:", "repo-key:", "pane:"];

fn is_machine_term(term: &str) -> bool {
    MACHINE_TERM_PREFIXES
        .iter()
        .any(|prefix| term.starts_with(prefix))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Project {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
    pub(crate) working_dir: String,
    #[serde(default)]
    pub(crate) tabs: Vec<ProjectTab>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ProjectTab {
    pub(crate) name: String,
    pub(crate) command: Option<String>,
    #[serde(default)]
    pub(crate) panes: Vec<ProjectPane>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ProjectPane {
    pub(crate) command: Option<String>,
    pub(crate) split: Option<String>,
    pub(crate) label: Option<String>,
}

impl ProjectTab {
    pub(crate) fn effective_panes(&self) -> Vec<ProjectPane> {
        if self.panes.is_empty() {
            return vec![ProjectPane {
                command: self.command.clone(),
                split: None,
                label: None,
            }];
        }

        self.panes
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, mut pane)| {
                pane.split = match (index, pane.split.as_deref()) {
                    (0, _) => None,
                    (_, Some(split)) if !split.is_empty() => Some(split.into()),
                    _ => Some("down".into()),
                };
                pane
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haystack_leaves_out_machine_only_search_terms() {
        let entry = Entry {
            source: Source::Workspace,
            title: "lager".into(),
            subtitle: String::new(),
            path: PathBuf::from("/repos/lager"),
            workspace_id: Some("w1".into()),
            workspace_label: Some("lager".into()),
            agent_target: None,
            action: EntryAction::FocusOrCreateDir,
            source_label: None,
            search_terms: vec![
                "w1".into(),
                "agent-name:claude".into(),
                "agent-status:idle".into(),
                "repo-key:/repos/lager/.git".into(),
                "pane:session:p1".into(),
            ],
        };
        let haystack = entry.haystack();
        assert!(haystack.contains("w1"));
        for hidden in ["claude", "idle", "repo-key", ".git", "pane:"] {
            assert!(!haystack.contains(hidden), "{hidden:?} in {haystack:?}");
        }
    }
}
