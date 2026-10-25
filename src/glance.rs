use serde::Deserialize;

use crate::{
    activity::ActivityState,
    topology::{AgentState, GitHead, WorkspaceNode},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum IconStyle {
    #[default]
    Unicode,
    Nerd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Group {
    Pinned,
    NeedsYou,
    Active,
    Recent,
    Quiet,
    Directories,
    Other,
}

impl Group {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned",
            Self::NeedsYou => "Needs you",
            Self::Active => "Active",
            Self::Recent => "Recent",
            Self::Quiet => "Quiet",
            Self::Directories => "Directories",
            Self::Other => "Other",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    Here,
    Last,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Thresholds {
    pub(crate) active_secs: i64,
    pub(crate) recent_secs: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneEvidence {
    pub(crate) state: AgentState,
    /// `None` when the recorded activity status no longer matches the live state.
    pub(crate) since: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Evidence {
    pub(crate) pinned: bool,
    pub(crate) agents: Vec<PaneEvidence>,
    pub(crate) focused_at: Option<i64>,
    pub(crate) last_commit_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Agent {
        state: AgentState,
        since: Option<i64>,
    },
    Commit(i64),
    Focus(i64),
    Age(i64),
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub(crate) group: Group,
    pub(crate) signal: Signal,
    pub(crate) last_active: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GlanceRow {
    pub(crate) group: Group,
    pub(crate) last_active: Option<i64>,
    pub(crate) state: AgentState,
    pub(crate) name: String,
    pub(crate) branch: String,
    pub(crate) repository: String,
    pub(crate) mark: Mark,
    pub(crate) signal: Signal,
    /// Whether an agent waits on the User, even when the row sits in another group such as Pinned.
    pub(crate) needs_you: bool,
}

pub(crate) fn evidence(
    workspace: &WorkspaceNode,
    activity: &ActivityState,
    pinned: bool,
) -> Evidence {
    let agents = workspace
        .tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .filter_map(|pane| {
            let agent = pane.agent.as_ref()?;
            let since = activity
                .pane(workspace.session.as_deref(), &pane.id)
                .filter(|recorded| AgentState::parse(&recorded.status) == agent.state)
                .map(|recorded| recorded.since);
            Some(PaneEvidence {
                state: agent.state,
                since,
            })
        })
        .collect();
    Evidence {
        pinned,
        agents,
        focused_at: activity.focused_at(workspace.session.as_deref(), &workspace.id),
        last_commit_at: workspace.git.as_ref().and_then(|git| git.last_commit_at),
    }
}

pub(crate) fn row(
    workspace: &WorkspaceNode,
    evidence: &Evidence,
    previous: bool,
    now: i64,
    thresholds: Thresholds,
) -> GlanceRow {
    let verdict = classify(evidence, now, thresholds);
    let state = evidence
        .agents
        .iter()
        .map(|agent| agent.state)
        .min_by_key(|state| precedence(*state))
        .unwrap_or(AgentState::Unknown);
    let branch = match workspace.git.as_ref().map(|git| &git.head) {
        Some(GitHead::Branch(branch)) | Some(GitHead::Detached(branch)) => branch.clone(),
        None => String::new(),
    };
    let mark = if workspace.focused {
        Mark::Here
    } else if previous {
        Mark::Last
    } else {
        Mark::None
    };
    GlanceRow {
        group: verdict.group,
        last_active: verdict.last_active,
        state,
        name: workspace.label.clone(),
        branch,
        repository: workspace
            .git
            .as_ref()
            .map_or_else(|| workspace.label.clone(), |git| git.repo_key.clone()),
        mark,
        signal: verdict.signal,
        needs_you: needs_you_signal(evidence).is_some(),
    }
}

pub(crate) fn classify(evidence: &Evidence, now: i64, thresholds: Thresholds) -> Verdict {
    let newest = newest_activity(evidence);
    let last_active = newest.as_ref().map(|(at, _)| *at);
    let activity_signal = newest.map(|(_, signal)| signal).unwrap_or(Signal::None);

    if evidence.pinned {
        return Verdict {
            group: Group::Pinned,
            signal: activity_signal,
            last_active,
        };
    }
    if let Some(signal) = needs_you_signal(evidence) {
        return Verdict {
            group: Group::NeedsYou,
            signal,
            last_active,
        };
    }
    if let Some(signal) = working_signal(evidence) {
        return Verdict {
            group: Group::Active,
            signal,
            last_active,
        };
    }
    let Some(at) = last_active else {
        return Verdict {
            group: Group::Quiet,
            signal: Signal::None,
            last_active,
        };
    };
    let age = (now - at).max(0);
    if age <= thresholds.active_secs {
        Verdict {
            group: Group::Active,
            signal: activity_signal,
            last_active,
        }
    } else if age <= thresholds.recent_secs {
        Verdict {
            group: Group::Recent,
            signal: activity_signal,
            last_active,
        }
    } else {
        Verdict {
            group: Group::Quiet,
            signal: Signal::Age(at),
            last_active,
        }
    }
}

fn precedence(state: AgentState) -> u8 {
    match state {
        AgentState::Blocked => 0,
        AgentState::Done => 1,
        AgentState::Working => 2,
        AgentState::Idle => 3,
        AgentState::Unknown => 4,
    }
}

fn newest_activity(evidence: &Evidence) -> Option<(i64, Signal)> {
    let agents = evidence.agents.iter().filter_map(|agent| {
        agent.since.map(|at| {
            (
                at,
                3,
                Signal::Agent {
                    state: agent.state,
                    since: Some(at),
                },
            )
        })
    });
    let commit = evidence
        .last_commit_at
        .map(|at| (at, 2, Signal::Commit(at)));
    let focus = evidence.focused_at.map(|at| (at, 1, Signal::Focus(at)));
    agents
        .chain(commit)
        .chain(focus)
        .max_by_key(|(at, priority, _)| (*at, *priority))
        .map(|(at, _, signal)| (at, signal))
}

fn needs_you_signal(evidence: &Evidence) -> Option<Signal> {
    evidence
        .agents
        .iter()
        .filter(|agent| matches!(agent.state, AgentState::Blocked | AgentState::Done))
        .min_by_key(|agent| (precedence(agent.state), std::cmp::Reverse(agent.since)))
        .map(|agent| Signal::Agent {
            state: agent.state,
            since: agent.since,
        })
}

fn working_signal(evidence: &Evidence) -> Option<Signal> {
    evidence
        .agents
        .iter()
        .filter(|agent| agent.state == AgentState::Working)
        .min_by_key(|agent| std::cmp::Reverse(agent.since))
        .map(|agent| Signal::Agent {
            state: agent.state,
            since: agent.since,
        })
}

/// The age of the activity that placed the row in its group. The detail pane names its source.
pub(crate) fn signal_text(signal: &Signal, now: i64) -> String {
    match signal {
        Signal::Agent {
            since: Some(at), ..
        }
        | Signal::Commit(at)
        | Signal::Focus(at)
        | Signal::Age(at) => age(now, *at),
        Signal::Agent { since: None, .. } | Signal::None => String::new(),
    }
}

pub(crate) fn age(now: i64, then: i64) -> String {
    let secs = (now - then).max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

const UNICODE_WORKING: [&str; 4] = ["◐", "◓", "◑", "◒"];
const NERD_WORKING: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn status_icon(state: AgentState, style: IconStyle, tick: u32) -> &'static str {
    let frame = tick as usize;
    match (style, state) {
        (_, AgentState::Unknown) => "",
        (IconStyle::Unicode, AgentState::Blocked) => "●",
        (IconStyle::Unicode, AgentState::Done) => "✔",
        (IconStyle::Unicode, AgentState::Working) => UNICODE_WORKING[frame % UNICODE_WORKING.len()],
        (IconStyle::Unicode, AgentState::Idle) => "○",
        (IconStyle::Nerd, AgentState::Blocked) => "\u{F009E}",
        (IconStyle::Nerd, AgentState::Done) => "\u{F05E0}",
        (IconStyle::Nerd, AgentState::Working) => NERD_WORKING[frame % NERD_WORKING.len()],
        (IconStyle::Nerd, AgentState::Idle) => "\u{F0766}",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLDS: Thresholds = Thresholds {
        active_secs: 15 * 60,
        recent_secs: 4 * 3_600,
    };
    const NOW: i64 = 1_000_000;

    fn agent(state: AgentState, since: Option<i64>) -> PaneEvidence {
        PaneEvidence { state, since }
    }

    fn evidence_with(agents: Vec<PaneEvidence>) -> Evidence {
        Evidence {
            agents,
            ..Evidence::default()
        }
    }

    #[test]
    fn pinned_workspace_stays_pinned_even_when_an_agent_needs_you() {
        let evidence = Evidence {
            pinned: true,
            ..evidence_with(vec![agent(AgentState::Blocked, Some(NOW))])
        };
        assert_eq!(classify(&evidence, NOW, THRESHOLDS).group, Group::Pinned);
    }

    #[test]
    fn blocked_and_done_agents_need_you() {
        for state in [AgentState::Blocked, AgentState::Done] {
            let verdict = classify(
                &evidence_with(vec![agent(state, Some(NOW - 240))]),
                NOW,
                THRESHOLDS,
            );
            assert_eq!(verdict.group, Group::NeedsYou, "{state:?}");
        }
    }

    #[test]
    fn needs_you_signal_prefers_blocked_over_done_and_the_latest_pane() {
        let evidence = evidence_with(vec![
            agent(AgentState::Done, Some(NOW - 60)),
            agent(AgentState::Blocked, Some(NOW - 240)),
            agent(AgentState::Blocked, Some(NOW - 10)),
        ]);
        assert_eq!(
            classify(&evidence, NOW, THRESHOLDS).signal,
            Signal::Agent {
                state: AgentState::Blocked,
                since: Some(NOW - 10),
            }
        );
    }

    #[test]
    fn blocked_agent_reads_as_waiting_in_the_signal() {
        let signal = Signal::Agent {
            state: AgentState::Blocked,
            since: Some(NOW - 240),
        };
        assert_eq!(signal_text(&signal, NOW), "4m");
    }

    #[test]
    fn working_agent_is_active_and_carries_its_own_signal() {
        let verdict = classify(
            &evidence_with(vec![agent(AgentState::Working, Some(NOW - 14 * 60))]),
            NOW,
            THRESHOLDS,
        );
        assert_eq!(verdict.group, Group::Active);
        assert_eq!(signal_text(&verdict.signal, NOW), "14m");
    }

    #[test]
    fn recent_idle_agent_counts_as_activity_by_its_since() {
        let verdict = classify(
            &evidence_with(vec![agent(AgentState::Idle, Some(NOW - 60))]),
            NOW,
            THRESHOLDS,
        );
        assert_eq!(verdict.group, Group::Active);
        let stale = classify(
            &evidence_with(vec![agent(AgentState::Idle, Some(NOW - 2 * 3_600))]),
            NOW,
            THRESHOLDS,
        );
        assert_eq!(stale.group, Group::Recent);
    }

    #[test]
    fn activity_at_the_active_boundary_is_active_and_one_second_later_is_recent() {
        let at_limit = Evidence {
            last_commit_at: Some(NOW - 15 * 60),
            ..Evidence::default()
        };
        let past_limit = Evidence {
            last_commit_at: Some(NOW - 15 * 60 - 1),
            ..Evidence::default()
        };
        assert_eq!(classify(&at_limit, NOW, THRESHOLDS).group, Group::Active);
        assert_eq!(classify(&past_limit, NOW, THRESHOLDS).group, Group::Recent);
    }

    #[test]
    fn activity_at_the_recent_boundary_is_recent_and_one_second_later_is_quiet() {
        let at_limit = Evidence {
            focused_at: Some(NOW - 4 * 3_600),
            ..Evidence::default()
        };
        let past_limit = Evidence {
            focused_at: Some(NOW - 4 * 3_600 - 1),
            ..Evidence::default()
        };
        assert_eq!(classify(&at_limit, NOW, THRESHOLDS).group, Group::Recent);
        assert_eq!(classify(&past_limit, NOW, THRESHOLDS).group, Group::Quiet);
    }

    #[test]
    fn newest_of_focus_commit_and_agent_since_decides_the_signal() {
        let focus_wins = Evidence {
            focused_at: Some(NOW - 60),
            last_commit_at: Some(NOW - 600),
            ..Evidence::default()
        };
        let commit_wins = Evidence {
            focused_at: Some(NOW - 600),
            last_commit_at: Some(NOW - 360),
            ..Evidence::default()
        };
        assert_eq!(
            signal_text(&classify(&focus_wins, NOW, THRESHOLDS).signal, NOW),
            "1m"
        );
        assert_eq!(
            signal_text(&classify(&commit_wins, NOW, THRESHOLDS).signal, NOW),
            "6m"
        );
    }

    #[test]
    fn quiet_rows_show_only_the_age_of_their_last_activity() {
        let evidence = Evidence {
            last_commit_at: Some(NOW - 9 * 86_400),
            ..Evidence::default()
        };
        let verdict = classify(&evidence, NOW, THRESHOLDS);
        assert_eq!(verdict.group, Group::Quiet);
        assert_eq!(signal_text(&verdict.signal, NOW), "9d");
    }

    #[test]
    fn workspace_without_any_timestamp_is_quiet_with_no_signal() {
        let verdict = classify(&Evidence::default(), NOW, THRESHOLDS);
        assert_eq!(verdict.group, Group::Quiet);
        assert_eq!(verdict.signal, Signal::None);
        assert_eq!(verdict.last_active, None);
    }

    #[test]
    fn future_timestamps_count_as_just_now() {
        let evidence = Evidence {
            focused_at: Some(NOW + 500),
            ..Evidence::default()
        };
        let verdict = classify(&evidence, NOW, THRESHOLDS);
        assert_eq!(verdict.group, Group::Active);
        assert_eq!(signal_text(&verdict.signal, NOW), "0s");
    }

    #[test]
    fn last_active_orders_rows_within_a_group() {
        let older = classify(
            &Evidence {
                last_commit_at: Some(NOW - 600),
                ..Evidence::default()
            },
            NOW,
            THRESHOLDS,
        );
        let newer = classify(
            &Evidence {
                focused_at: Some(NOW - 60),
                ..Evidence::default()
            },
            NOW,
            THRESHOLDS,
        );
        assert!(newer.last_active > older.last_active);
    }

    #[test]
    fn ages_use_seconds_minutes_hours_and_days() {
        assert_eq!(age(NOW, NOW - 48), "48s");
        assert_eq!(age(NOW, NOW - 59), "59s");
        assert_eq!(age(NOW, NOW - 60), "1m");
        assert_eq!(age(NOW, NOW - 3_599), "59m");
        assert_eq!(age(NOW, NOW - 3_600), "1h");
        assert_eq!(age(NOW, NOW - 86_399), "23h");
        assert_eq!(age(NOW, NOW - 86_400), "1d");
    }

    #[test]
    fn unicode_icons_cycle_only_while_working() {
        assert_eq!(status_icon(AgentState::Blocked, IconStyle::Unicode, 0), "●");
        assert_eq!(status_icon(AgentState::Blocked, IconStyle::Unicode, 3), "●");
        assert_eq!(status_icon(AgentState::Done, IconStyle::Unicode, 0), "✔");
        assert_eq!(status_icon(AgentState::Idle, IconStyle::Unicode, 9), "○");
        assert_eq!(status_icon(AgentState::Unknown, IconStyle::Unicode, 0), "");
        let frames: Vec<_> = (0..5)
            .map(|tick| status_icon(AgentState::Working, IconStyle::Unicode, tick))
            .collect();
        assert_eq!(frames, ["◐", "◓", "◑", "◒", "◐"]);
    }

    #[test]
    fn nerd_icons_use_the_nerd_font_glyphs() {
        assert_eq!(
            status_icon(AgentState::Blocked, IconStyle::Nerd, 0),
            "\u{F009E}"
        );
        assert_eq!(
            status_icon(AgentState::Done, IconStyle::Nerd, 0),
            "\u{F05E0}"
        );
        assert_eq!(
            status_icon(AgentState::Idle, IconStyle::Nerd, 0),
            "\u{F0766}"
        );
        assert_eq!(status_icon(AgentState::Unknown, IconStyle::Nerd, 0), "");
        let frames: Vec<_> = (0..11)
            .map(|tick| status_icon(AgentState::Working, IconStyle::Nerd, tick))
            .collect();
        assert_eq!(&frames[..3], ["⠋", "⠙", "⠹"]);
        assert_eq!(frames[10], "⠋");
    }
}
