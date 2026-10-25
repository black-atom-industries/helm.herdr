use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    time::UNIX_EPOCH,
};

use crate::topology::OpenTopology;

/// A subagent counts as running while its transcript changed this recently.
pub(crate) const RUNNING_WINDOW_SECS: i64 = 600;

/// Bytes read from the end of a transcript. The newest PROGRESS line sits there.
const TAIL_BYTES: u64 = 256 * 1024;

/// Bytes read from the start of a transcript, enough to reach its first line.
const HEAD_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GitDetail {
    pub(crate) changes: Vec<Change>,
    pub(crate) commits: Vec<Commit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub(crate) state: char,
    pub(crate) path: String,
    pub(crate) added: Option<u32>,
    pub(crate) removed: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Commit {
    pub(crate) hash: String,
    pub(crate) subject: String,
    pub(crate) committed_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Subagent {
    pub(crate) step: String,
    pub(crate) done: u32,
    pub(crate) total: u32,
    pub(crate) started_at: i64,
}

/// A git read kept per workspace, with the time it was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedGit {
    pub(crate) detail: GitDetail,
    pub(crate) fetched_at: i64,
}

/// Reads the changes and recent commits of the repository at `cwd`. A failing
/// git call, such as `diff HEAD` before the first commit, counts as empty.
pub(crate) fn read_git(cwd: &Path) -> GitDetail {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    };
    GitDetail {
        changes: changes(
            &git(&["status", "--porcelain=v1", "-z"]),
            &git(&["diff", "--numstat", "-z", "--no-renames", "HEAD"]),
        ),
        commits: parse_log(&git(&["log", "-8", "--format=%h%x09%s%x09%ct"])),
    }
}

fn changes(porcelain: &str, numstat: &str) -> Vec<Change> {
    let counts = parse_numstat(numstat);
    parse_porcelain(porcelain)
        .into_iter()
        .map(|(state, path)| {
            let (added, removed) = counts.get(&path).copied().unwrap_or((None, None));
            Change {
                state,
                path,
                added,
                removed,
            }
        })
        .collect()
}

/// Reads NUL-terminated `XY path` records. The first column wins over the
/// second unless it is blank. A rename or copy record is followed by its origin
/// path, which is skipped.
fn parse_porcelain(output: &str) -> Vec<(char, String)> {
    let mut entries = Vec::new();
    let mut fields = output.split('\0').filter(|field| !field.is_empty());
    while let Some(field) = fields.next() {
        let Some(code) = field.get(..2) else {
            continue;
        };
        let path = field.get(3..).unwrap_or_default();
        let state = match code {
            "??" => '?',
            _ => code
                .chars()
                .find(|c| *c != ' ')
                .filter(|c| matches!(c, 'A' | 'D' | 'R'))
                .unwrap_or('M'),
        };
        if code.contains(['R', 'C']) {
            fields.next();
        }
        entries.push((state, path.to_string()));
    }
    entries
}

/// Reads NUL-terminated `added<TAB>removed<TAB>path` records. Binary files have no counts.
fn parse_numstat(output: &str) -> HashMap<String, (Option<u32>, Option<u32>)> {
    output
        .split('\0')
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next()?.parse().ok();
            let removed = fields.next()?.parse().ok();
            Some((fields.next()?.to_string(), (added, removed)))
        })
        .collect()
}

/// Reads `hash<TAB>subject<TAB>committer-unix-time` lines.
fn parse_log(output: &str) -> Vec<Commit> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            Some(Commit {
                hash: fields.next()?.to_string(),
                subject: fields.next()?.to_string(),
                committed_at: fields.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// Claude names a session's project directory after its working directory,
/// with every character that is not ASCII alphanumeric replaced by `-`.
/// Subagent transcripts sit below the session.
pub(crate) fn subagent_dir(projects: &Path, cwd: &Path, session: &str) -> PathBuf {
    let project: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    projects.join(project).join(session).join("subagents")
}

/// Subagents in `dir` that changed within `RUNNING_WINDOW_SECS` and whose last
/// PROGRESS line is short of its total. Stale files are skipped unread.
pub(crate) fn running_subagents(dir: &Path, now: i64) -> Vec<Subagent> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("agent-") && name.ends_with(".jsonl")
        })
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let modified = metadata
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_secs() as i64;
            if now - modified > RUNNING_WINDOW_SECS {
                return None;
            }
            let tail = read_tail(&entry.path(), metadata.len(), TAIL_BYTES)?;
            let (done, total, step) = last_progress(&tail)?;
            if done >= total {
                return None;
            }
            let started_at = read_head(&entry.path())
                .and_then(|head| first_timestamp(&head))
                .unwrap_or_else(|| metadata.ctime());
            Some(Subagent {
                step,
                done,
                total,
                started_at,
            })
        })
        .collect()
}

/// Running subagents of each Claude pane, keyed by pane id.
pub(crate) fn scan_subagents(
    topology: &OpenTopology,
    projects: &Path,
    now: i64,
) -> HashMap<String, Vec<Subagent>> {
    topology
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| &tab.panes)
        .filter(|pane| {
            pane.agent
                .as_ref()
                .is_some_and(|agent| agent.name.eq_ignore_ascii_case("claude"))
        })
        .filter_map(|pane| {
            let session = pane.agent_session.as_deref()?;
            let running = running_subagents(&subagent_dir(projects, &pane.cwd, session), now);
            (!running.is_empty()).then(|| (pane.id.clone(), running))
        })
        .collect()
}

/// The last `window` bytes of a transcript of `len` bytes. A partial first line is dropped.
fn read_tail(path: &Path, len: u64, window: u64) -> Option<String> {
    let start = len.saturating_sub(window);
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(window).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    if start == 0 {
        return Some(text.into_owned());
    }
    Some(
        text.find('\n')
            .map_or_else(String::new, |at| text[at + 1..].to_string()),
    )
}

fn read_head(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(HEAD_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn last_progress(text: &str) -> Option<(u32, u32, String)> {
    text.lines()
        .filter(|line| line.contains("\"type\":\"assistant\""))
        .filter_map(progress_in_line)
        .next_back()
}

/// The last `PROGRESS: n/m · step` on a line. The step ends at the JSON string.
fn progress_in_line(line: &str) -> Option<(u32, u32, String)> {
    const MARKER: &str = "PROGRESS: ";
    line.rmatch_indices(MARKER).find_map(|(start, _)| {
        let rest = &line[start + MARKER.len()..];
        let (ratio, step) = rest.split_once(" · ")?;
        let (done, total) = ratio.split_once('/')?;
        let step: String = step
            .chars()
            .take_while(|c| *c != '"' && *c != '\\')
            .collect();
        Some((
            done.parse().ok()?,
            total.parse().ok()?,
            step.trim_end().to_string(),
        ))
    })
}

fn first_timestamp(text: &str) -> Option<i64> {
    const KEY: &str = "\"timestamp\":\"";
    let first = text.lines().next()?;
    let start = first.find(KEY)? + KEY.len();
    epoch_seconds(&first[start..])
}

/// Seconds since the Unix epoch of a UTC `YYYY-MM-DDTHH:MM:SS…` timestamp.
fn epoch_seconds(stamp: &str) -> Option<i64> {
    let field = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    let days = days_from_civil(field(0..4)?, field(5..7)?, field(8..10)?);
    Some(days * 86_400 + field(11..13)? * 3_600 + field(14..16)? * 60 + field(17..19)?)
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        process,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("helm-detail-{}-{nanos}-{name}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    #[test]
    fn porcelain_and_numstat_build_the_changes_list() {
        let porcelain = " M lua/mdn/lists.lua\0A  tests/lists_spec.lua\0 D old.md\0?? notes.txt\0";
        let numstat = "42\t12\tlua/mdn/lists.lua\x0031\t0\ttests/lists_spec.lua\x000\t9\told.md\0";
        let changes = changes(porcelain, numstat);
        assert_eq!(
            changes,
            vec![
                Change {
                    state: 'M',
                    path: "lua/mdn/lists.lua".into(),
                    added: Some(42),
                    removed: Some(12),
                },
                Change {
                    state: 'A',
                    path: "tests/lists_spec.lua".into(),
                    added: Some(31),
                    removed: Some(0),
                },
                Change {
                    state: 'D',
                    path: "old.md".into(),
                    added: Some(0),
                    removed: Some(9),
                },
                Change {
                    state: '?',
                    path: "notes.txt".into(),
                    added: None,
                    removed: None,
                },
            ]
        );
    }

    #[test]
    fn renames_keep_the_new_path_and_binary_counts_stay_blank() {
        let porcelain = "R  lua/new.lua\0old.lua\0 M my notes.md\0";
        let numstat = "-\t-\tlua/new.lua\x005\t1\tmy notes.md\0";
        assert_eq!(
            changes(porcelain, numstat),
            vec![
                Change {
                    state: 'R',
                    path: "lua/new.lua".into(),
                    added: None,
                    removed: None,
                },
                Change {
                    state: 'M',
                    path: "my notes.md".into(),
                    added: Some(5),
                    removed: Some(1),
                },
            ]
        );
    }

    #[test]
    fn log_lines_parse_into_commits() {
        let output = "3f1a9c0\tCycle checkboxes in visual mode\t1792931924\nb82e4d1\tKeep indentation\t1792845524\n";
        assert_eq!(
            parse_log(output),
            vec![
                Commit {
                    hash: "3f1a9c0".into(),
                    subject: "Cycle checkboxes in visual mode".into(),
                    committed_at: 1_792_931_924,
                },
                Commit {
                    hash: "b82e4d1".into(),
                    subject: "Keep indentation".into(),
                    committed_at: 1_792_845_524,
                },
            ]
        );
    }

    #[test]
    fn subagent_dir_encodes_the_working_directory_like_claude() {
        let dir = subagent_dir(
            Path::new("/home/u/.claude/projects"),
            Path::new("/Users/brunner/repos/black-atom-industries/helm.herdr"),
            "16f766cd",
        );
        assert_eq!(
            dir,
            PathBuf::from(
                "/home/u/.claude/projects/-Users-brunner-repos-black-atom-industries-helm-herdr/16f766cd/subagents"
            )
        );
    }

    #[test]
    fn last_progress_reads_only_assistant_lines_and_keeps_the_final_one() {
        let text = [
            r#"{"type":"user","message":{"content":"PROGRESS: 9/9 · example from the hook"}}"#,
            r#"{"type":"assistant","message":{"content":"PROGRESS: 2/5 · Port the tests"}}"#,
            r#"{"type":"assistant","message":{"content":"PROGRESS: 4/5 · Port the **list** tests \"ok\""}}"#,
        ]
        .join("\n");
        assert_eq!(
            last_progress(&text),
            Some((4, 5, "Port the **list** tests".into()))
        );
    }

    #[test]
    fn epoch_seconds_matches_known_dates() {
        assert_eq!(epoch_seconds("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_seconds("2000-03-01T00:00:00.000Z"), Some(951_868_800));
        assert_eq!(
            epoch_seconds("2026-10-18T14:24:01.467Z"),
            Some(1_792_281_600 + 14 * 3_600 + 24 * 60 + 1)
        );
    }

    #[test]
    fn tail_read_drops_the_partial_first_line() {
        let dir = scratch_dir("tail");
        let path = dir.join("agent-a.jsonl");
        let progress =
            r#"{"type":"assistant","message":{"content":"PROGRESS: 2/5 · Port the tests"}}"#;
        let text = format!("{}\n{progress}\n", "x".repeat(200));
        fs::write(&path, &text).unwrap();
        let tail = read_tail(&path, text.len() as u64, 120).unwrap();
        assert!(!tail.contains('x'));
        assert_eq!(last_progress(&tail), Some((2, 5, "Port the tests".into())));

        let text = format!("{progress}\n{}\n", "y".repeat(200));
        fs::write(&path, &text).unwrap();
        let tail = read_tail(&path, text.len() as u64, 120).unwrap();
        assert_eq!(last_progress(&tail), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn running_subagents_skip_finished_and_stale_transcripts() {
        let dir = scratch_dir("running");
        let running = r#"{"type":"user","timestamp":"2000-03-01T00:00:00.000Z"}
{"type":"assistant","message":{"content":"PROGRESS: 3/6 · Research markers"}}
"#;
        let finished = r#"{"type":"assistant","message":{"content":"PROGRESS: 4/4 · Done"}}
"#;
        fs::write(dir.join("agent-a.jsonl"), running).unwrap();
        fs::write(dir.join("agent-b.jsonl"), finished).unwrap();
        fs::write(dir.join("other.jsonl"), running).unwrap();

        let found = running_subagents(&dir, now());
        assert_eq!(
            found,
            vec![Subagent {
                step: "Research markers".into(),
                done: 3,
                total: 6,
                started_at: 951_868_800,
            }]
        );
        assert!(running_subagents(&dir, now() + RUNNING_WINDOW_SECS + 1).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
