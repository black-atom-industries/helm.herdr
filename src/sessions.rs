use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use crate::{
    activity::ActivityState,
    glance::{self, IconStyle},
    herdr::socket_request_at,
    paths::{herdr_root, plugin_config_dir},
    topology::{self, AgentState, OpenTopology},
};

/// How long a session's socket may take to answer a snapshot before it counts as stopped.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(300);

/// A Herdr session: the default one in Herdr's root, or a named one in `<root>/sessions/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Session {
    /// `None` for the default session.
    pub(crate) name: Option<String>,
    pub(crate) dir: PathBuf,
}

impl Session {
    /// The session Herdr calls `label` (`default` for the default session) under `root`.
    pub(crate) fn at(root: &Path, label: &str) -> Self {
        match label {
            "default" => Session {
                name: None,
                dir: root.to_path_buf(),
            },
            name => Session {
                name: Some(name.to_string()),
                dir: root.join("sessions").join(name),
            },
        }
    }

    pub(crate) fn socket(&self) -> PathBuf {
        self.dir.join("herdr.sock")
    }

    /// The name Herdr shows for the session.
    pub(crate) fn label(name: Option<&str>) -> &str {
        name.unwrap_or("default")
    }

    /// The live snapshot, shaped like `herdr api snapshot` output, or `None`
    /// when the socket does not answer in time.
    fn snapshot(&self) -> Option<Value> {
        socket_request_at(
            &self.socket(),
            &format!("helm-herdr-{}", std::process::id()),
            "session.snapshot",
            serde_json::json!({}),
            Some(SNAPSHOT_TIMEOUT),
        )
        .ok()
        .map(|result| serde_json::json!({ "result": result }))
    }
}

/// The session this process runs in, from `HERDR_SESSION`; `None` is the default session.
pub(crate) fn current() -> Option<String> {
    env::var("HERDR_SESSION")
        .ok()
        .filter(|name| !name.trim().is_empty())
}

/// The default session plus every directory in `<root>/sessions/`, symlinks
/// followed, named sessions sorted by name.
pub(crate) fn discover(root: &Path) -> Vec<Session> {
    let mut named: Vec<Session> = fs::read_dir(root.join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            Some(Session {
                name: Some(entry.file_name().into_string().ok()?),
                dir: entry.path(),
            })
        })
        .collect();
    named.sort_by(|a, b| a.name.cmp(&b.name));
    let default = Session {
        name: None,
        dir: root.to_path_buf(),
    };
    std::iter::once(default).chain(named).collect()
}

/// Whether `session` is the one named `current`. Herdr calls the default
/// session `default`, so that name and `None` are the same session.
fn is_current(session: &Session, current: Option<&str>, current_socket: Option<&Path>) -> bool {
    fn named(name: Option<&str>) -> Option<&str> {
        name.filter(|name| *name != "default")
    }
    named(session.name.as_deref()) == named(current)
        || current_socket.is_some_and(|socket| socket == session.socket())
}

/// Each session that answers a snapshot in time, with that snapshot, in
/// discovery order. Each socket is asked on its own detached thread, and
/// answers after the deadline are dropped, so even a blocking connect costs
/// at most one timeout.
pub(crate) fn running(sessions: Vec<Session>) -> Vec<(Session, Value)> {
    let (sender, receiver) = mpsc::channel();
    let count = sessions.len();
    for (index, session) in sessions.into_iter().enumerate() {
        let sender = sender.clone();
        thread::spawn(move || {
            let read = session.snapshot().map(|value| (session, value));
            let _ = sender.send((index, read));
        });
    }
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    let mut found = Vec::new();
    for _ in 0..count {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        match receiver.recv_timeout(left) {
            Ok((index, Some(read))) => found.push((index, read)),
            Ok((_, None)) => {}
            Err(_) => break,
        }
    }
    found.sort_by_key(|(index, _)| *index);
    found.into_iter().map(|(_, read)| read).collect()
}

/// Every running session other than the current one, with its snapshot.
fn other_running(
    root: &Path,
    current: Option<&str>,
    current_socket: Option<&Path>,
) -> Vec<(Session, Value)> {
    let others = discover(root)
        .into_iter()
        .filter(|session| !is_current(session, current, current_socket))
        .collect();
    running(others)
}

/// A discovered session and, while it runs, its Workspaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionState {
    pub(crate) session: Session,
    pub(crate) current: bool,
    /// `None` while the session is stopped. The current session's Workspaces
    /// are the local Topology, so its own stays empty.
    pub(crate) topology: Option<OpenTopology>,
}

/// Every discovered session in discovery order, the current one included
/// even when its directory is missing.
pub(crate) fn states(
    root: &Path,
    current: Option<&str>,
    current_socket: Option<&Path>,
) -> Vec<SessionState> {
    let (mine, others): (Vec<Session>, Vec<Session>) = discover(root)
        .into_iter()
        .partition(|session| is_current(session, current, current_socket));
    let answered = running(others.clone());
    let mut states: Vec<SessionState> = others
        .into_iter()
        .map(|session| {
            let topology = answered
                .iter()
                .find(|(running, _)| *running == session)
                .map(|(_, snapshot)| topology::parse_snapshot(snapshot, session.name.clone()));
            SessionState {
                session,
                current: false,
                topology,
            }
        })
        .collect();
    let mine = mine.into_iter().next().unwrap_or_else(|| {
        let mut session = Session::at(root, current.unwrap_or("default"));
        if let Some(dir) = current_socket.and_then(Path::parent) {
            session.dir = dir.to_path_buf();
        }
        session
    });
    states.insert(
        0,
        SessionState {
            session: mine,
            current: true,
            topology: Some(OpenTopology::default()),
        },
    );
    states
}

/// The Workspaces of every running session other than the current one.
pub(crate) fn other_topology(states: &[SessionState]) -> OpenTopology {
    OpenTopology {
        workspaces: states
            .iter()
            .filter(|state| !state.current)
            .filter_map(|state| state.topology.as_ref())
            .flat_map(|topology| topology.workspaces.iter().cloned())
            .collect(),
    }
}

/// A running session with Workspaces in Needs you.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    pub(crate) session: String,
    pub(crate) state: AgentState,
    pub(crate) count: usize,
}

/// The other running sessions that have Workspaces in Needs you, in discovery order.
pub(crate) fn waiting(
    root: &Path,
    current: Option<&str>,
    current_socket: Option<&Path>,
) -> Vec<Waiting> {
    let activity = ActivityState::default();
    other_running(root, current, current_socket)
        .into_iter()
        .filter_map(|(session, snapshot)| {
            let states: Vec<AgentState> = topology::parse_snapshot(&snapshot, session.name.clone())
                .workspaces
                .iter()
                .filter_map(|workspace| {
                    glance::needs_you_state(&glance::evidence(workspace, &activity, false, false))
                })
                .collect();
            let state = glance::most_urgent(states.iter().copied())?;
            Some(Waiting {
                session: Session::label(session.name.as_deref()).to_string(),
                state,
                count: states.len(),
            })
        })
        .collect()
}

/// `<session> <icon><count>` per waiting session, joined by two spaces.
pub(crate) fn status_line(waiting: &[Waiting], icons: IconStyle) -> String {
    waiting
        .iter()
        .map(|entry| {
            format!(
                "{} {}{}",
                entry.session,
                glance::status_icon(entry.state, icons, 0),
                entry.count
            )
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// [`waiting`] for this process's Herdr root and session.
pub(crate) fn read_waiting() -> Vec<Waiting> {
    let socket = env::var_os("HERDR_SOCKET_PATH").map(PathBuf::from);
    waiting(&herdr_root(), current().as_deref(), socket.as_deref())
}

/// [`states`] for this process's Herdr root and session.
pub(crate) fn read_states() -> Vec<SessionState> {
    let socket = env::var_os("HERDR_SOCKET_PATH").map(PathBuf::from);
    states(&herdr_root(), current().as_deref(), socket.as_deref())
}

/// A running `helm-herdr shell` and the Herdr client it waits for, stored in
/// `launchers/<session>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LauncherRecord {
    pub(crate) launcher_pid: u32,
    pub(crate) client_pid: u32,
    pub(crate) session: String,
}

/// The session a launcher attaches once its client exits, stored in
/// `launchers/switch-<launcher_pid>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SwitchRequest {
    pub(crate) session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tab_id: Option<String>,
}

pub(crate) fn launchers_dir() -> PathBuf {
    plugin_config_dir().join("launchers")
}

fn record_path(dir: &Path, session: &str) -> PathBuf {
    dir.join(format!("{session}.json"))
}

fn switch_path(dir: &Path, launcher_pid: u32) -> PathBuf {
    dir.join(format!("switch-{launcher_pid}.json"))
}

/// The socket of the session Herdr calls `session`.
pub(crate) fn socket_of(root: &Path, session: &str) -> PathBuf {
    Session::at(root, session).socket()
}

/// Refuses names that would leave `launchers/` or collide with a switch request file.
pub(crate) fn check_session_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name.contains("..") || name.starts_with("switch-") {
        return Err(format!("invalid Herdr session name: {name:?}"));
    }
    Ok(())
}

/// Writes through a temporary file and a rename, so readers never see half a file.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    fs::write(&temp, contents)?;
    fs::rename(&temp, path)
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let encoded = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(path, &encoded)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub(crate) fn write_record(dir: &Path, record: &LauncherRecord) -> io::Result<()> {
    write_json(&record_path(dir, &record.session), record)
}

/// Removes `record`'s file unless another launcher has written its own since.
pub(crate) fn remove_record(dir: &Path, record: &LauncherRecord) {
    let path = record_path(dir, &record.session);
    if read_json::<LauncherRecord>(&path)
        .is_some_and(|stored| stored.launcher_pid == record.launcher_pid)
    {
        let _ = fs::remove_file(path);
    }
}

/// The switch request addressed to `launcher_pid`, removed as it is read.
pub(crate) fn take_switch_request(dir: &Path, launcher_pid: u32) -> Option<SwitchRequest> {
    let path = switch_path(dir, launcher_pid);
    let request: Option<SwitchRequest> = read_json(&path);
    let _ = fs::remove_file(path);
    request.filter(|request| check_session_name(&request.session).is_ok())
}

/// Drops a switch request left by an earlier process with the same pid.
pub(crate) fn clear_switch_request(dir: &Path, launcher_pid: u32) {
    let _ = fs::remove_file(switch_path(dir, launcher_pid));
}

/// The record for `session` in `dir`, unless its launcher is gone or its
/// client is no longer the launcher's `herdr` child.
pub(crate) fn launcher_at(dir: &Path, session: &str) -> Option<LauncherRecord> {
    let record: LauncherRecord = read_json(&record_path(dir, session))?;
    (process_alive(record.launcher_pid) && is_client_of(record.client_pid, record.launcher_pid))
        .then_some(record)
}

/// Asks the launcher behind `record` to attach `target`: writes the request,
/// then ends the launcher's client with SIGTERM.
pub(crate) fn request_switch_at(
    dir: &Path,
    record: &LauncherRecord,
    target: &SwitchRequest,
) -> Result<(), String> {
    check_session_name(&target.session)?;
    let path = switch_path(dir, record.launcher_pid);
    write_json(&path, target)
        .map_err(|error| format!("failed to write switch request: {error}"))?;
    if !is_client_of(record.client_pid, record.launcher_pid) {
        let _ = fs::remove_file(path);
        return Err("the launcher's Herdr client is gone".into());
    }
    terminate(record.client_pid).map_err(|error| {
        let _ = fs::remove_file(&path);
        format!("failed to end the Herdr client: {error}")
    })
}

/// Whether `pid` is a running `herdr` process whose parent is `parent`.
fn is_client_of(pid: u32, parent: u32) -> bool {
    let Ok(output) = Command::new("ps")
        .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some((ppid, command)) = text.trim().split_once(char::is_whitespace) else {
        return false;
    };
    ppid.trim().parse() == Ok(parent)
        && Path::new(command.trim())
            .file_name()
            .is_some_and(|name| name == "herdr")
}

#[cfg(unix)]
mod signals {
    use std::io;

    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    const SIG_DFL: usize = 0;
    const SIG_IGN: usize = 1;
    const EPERM: i32 = 1;

    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
        fn signal(signal: i32, handler: usize) -> usize;
    }

    /// A pid that names one process; 0 and negative values address groups.
    fn single(pid: u32) -> Option<i32> {
        i32::try_from(pid).ok().filter(|pid| *pid > 0)
    }

    pub(crate) fn process_alive(pid: u32) -> bool {
        let Some(pid) = single(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks that the process exists.
        let found = unsafe { kill(pid, 0) } == 0;
        found || io::Error::last_os_error().raw_os_error() == Some(EPERM)
    }

    pub(crate) fn terminate(pid: u32) -> io::Result<()> {
        let pid = single(pid).ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: plain kill(2) on one process.
        if unsafe { kill(pid, SIGTERM) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// While `ignore` holds, Ctrl-C in the terminal leaves this process running.
    pub(crate) fn ignore_interrupt(ignore: bool) {
        // SAFETY: installs SIG_IGN or SIG_DFL, no handler code runs.
        unsafe {
            signal(SIGINT, if ignore { SIG_IGN } else { SIG_DFL });
        }
    }
}

#[cfg(not(unix))]
mod signals {
    pub(crate) fn process_alive(_pid: u32) -> bool {
        false
    }

    pub(crate) fn terminate(_pid: u32) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    pub(crate) fn ignore_interrupt(_ignore: bool) {}
}

pub(crate) use signals::ignore_interrupt;
use signals::{process_alive, terminate};

/// Child processes for tests that need a live launcher or Herdr client.
#[cfg(all(test, unix))]
pub(crate) mod test_processes {
    use std::{path::Path, process::Command};

    /// A child process that is killed and reaped when the test ends, also on a failed assert.
    pub(crate) struct Kid(pub(crate) std::process::Child);

    impl Drop for Kid {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    impl std::ops::Deref for Kid {
        type Target = std::process::Child;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl std::ops::DerefMut for Kid {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    /// A `sleep` that `ps` names `herdr`, started as this process's child.
    pub(crate) fn fake_client(dir: &Path) -> Kid {
        let link = dir.join("herdr");
        if !link.exists() {
            std::os::unix::fs::symlink("/bin/sleep", &link).unwrap();
        }
        Kid(Command::new(link).arg("30").spawn().unwrap())
    }

    pub(crate) fn sleeper() -> Kid {
        Kid(Command::new("sleep").arg("30").spawn().unwrap())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::test_processes::{fake_client, sleeper};
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        time::Instant,
    };

    /// A short temp root: Unix socket paths are limited to about 100 bytes.
    fn temp_root(tag: &str) -> PathBuf {
        let root = env::temp_dir().join(format!("hs{tag}{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sessions")).unwrap();
        root
    }

    fn snapshot_result(workspace_label: &str, agent_status: &str) -> Value {
        serde_json::json!({
            "type": "session_snapshot",
            "snapshot": {
                "workspaces": [{"workspace_id": "w1", "label": workspace_label, "focused": true}],
                "tabs": [{"tab_id": "w1:t1", "workspace_id": "w1", "label": "1", "focused": true}],
                "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1", "cwd": "/tmp", "focused": true}],
                "agents": [{"pane_id": "w1:p1", "agent": "claude", "agent_status": agent_status}],
            }
        })
    }

    /// Serves one snapshot request on `<dir>/herdr.sock`.
    fn serve(dir: &Path, result: Value) -> thread::JoinHandle<()> {
        let listener = UnixListener::bind(dir.join("herdr.sock")).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "session.snapshot");
            let response = serde_json::json!({"id": request["id"], "result": result});
            writeln!(stream, "{response}").unwrap();
        })
    }

    #[test]
    fn discovery_lists_the_default_session_and_every_session_directory() {
        let _lock = crate::herdr::test_env_lock();
        let root = temp_root("d");
        let target = env::temp_dir().join(format!("hsdt{}", std::process::id()));
        let _ = fs::remove_dir_all(&target);
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(root.join("sessions/plain")).unwrap();
        std::os::unix::fs::symlink(&target, root.join("sessions/linked")).unwrap();
        fs::write(root.join("sessions/stray-file"), "").unwrap();

        let sessions = discover(&root);
        let names: Vec<Option<&str>> = sessions.iter().map(|s| s.name.as_deref()).collect();
        assert_eq!(names, [None, Some("linked"), Some("plain")]);
        assert_eq!(sessions[0].socket(), root.join("herdr.sock"));
        assert_eq!(
            sessions[1].socket(),
            root.join("sessions/linked/herdr.sock")
        );

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn a_session_runs_only_when_its_socket_answers_a_snapshot() {
        let _lock = crate::herdr::test_env_lock();
        let root = temp_root("r");
        let target = env::temp_dir().join(format!("hsrt{}", std::process::id()));
        let _ = fs::remove_dir_all(&target);
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(root.join("sessions/plain")).unwrap();
        fs::create_dir_all(root.join("sessions/stopped")).unwrap();
        std::os::unix::fs::symlink(&target, root.join("sessions/linked")).unwrap();
        // A socket file nobody listens on any more.
        drop(UnixListener::bind(root.join("sessions/stopped/herdr.sock")).unwrap());

        let default = serve(&root, snapshot_result("home", "idle"));
        let plain = serve(&root.join("sessions/plain"), snapshot_result("api", "idle"));
        let linked = serve(&target, snapshot_result("docs", "idle"));

        let running: Vec<Option<String>> = running(discover(&root))
            .into_iter()
            .map(|(session, _)| session.name)
            .collect();
        assert_eq!(running, [None, Some("linked".into()), Some("plain".into())]);
        for server in [default, plain, linked] {
            server.join().unwrap();
        }

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn other_sessions_skip_the_current_one_and_carry_their_session_name() {
        let _lock = crate::herdr::test_env_lock();
        let root = temp_root("o");
        fs::create_dir_all(root.join("sessions/work")).unwrap();
        let work = serve(
            &root.join("sessions/work"),
            snapshot_result("api", "blocked"),
        );

        let topology = other_topology(&states(&root, Some("current"), None));
        work.join().unwrap();
        let workspace = &topology.workspaces[0];
        assert_eq!(topology.workspaces.len(), 1);
        assert_eq!(workspace.label, "api");
        assert_eq!(workspace.session.as_deref(), Some("work"));
        assert_eq!(
            workspace.tabs[0].panes[0].agent.as_ref().map(|a| a.state),
            Some(topology::AgentState::Blocked)
        );

        let default = Session {
            name: None,
            dir: root.clone(),
        };
        assert!(is_current(&default, None, None));
        assert!(is_current(&default, Some("default"), None));
        assert!(!is_current(&default, Some("work"), None));
        assert!(is_current(
            &default,
            Some("work"),
            Some(&root.join("herdr.sock"))
        ));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn states_list_every_session_with_the_current_one_first() {
        let _lock = crate::herdr::test_env_lock();
        let root = temp_root("s");
        fs::create_dir_all(root.join("sessions/a")).unwrap();
        fs::create_dir_all(root.join("sessions/b")).unwrap();
        let a = serve(&root.join("sessions/a"), snapshot_result("api", "idle"));

        let found = states(&root, Some("b"), None);
        a.join().unwrap();
        let names: Vec<(Option<&str>, bool, bool)> = found
            .iter()
            .map(|state| {
                (
                    state.session.name.as_deref(),
                    state.current,
                    state.topology.is_some(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                (Some("b"), true, true),
                (None, false, false),
                (Some("a"), false, true),
            ]
        );
        assert_eq!(
            found[2].topology.as_ref().unwrap().workspaces[0].label,
            "api"
        );
        assert_eq!(other_topology(&found).workspaces.len(), 1);

        let gone = states(&root, Some("gone"), None);
        assert!(gone[0].current);
        assert_eq!(gone[0].session.name.as_deref(), Some("gone"));
        assert_eq!(gone[0].session.dir, root.join("sessions/gone"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn status_line_joins_sessions_with_icon_and_count() {
        let waiting = [
            Waiting {
                session: "b".into(),
                state: AgentState::Blocked,
                count: 2,
            },
            Waiting {
                session: "default".into(),
                state: AgentState::Done,
                count: 1,
            },
        ];
        assert_eq!(
            status_line(&waiting, IconStyle::Unicode),
            "b ●2  default ✔1"
        );
        assert_eq!(status_line(&[], IconStyle::Unicode), "");
    }

    #[test]
    fn waiting_counts_needs_you_per_other_session_and_skips_the_current_one() {
        let _lock = crate::herdr::test_env_lock();
        let root = temp_root("w");
        let mut servers = Vec::new();
        for (name, status) in [("a", "blocked"), ("b", "done"), ("c", "idle")] {
            let dir = root.join("sessions").join(name);
            fs::create_dir_all(&dir).unwrap();
            servers.push(serve(&dir, snapshot_result("api", status)));
        }

        let waiting = waiting(&root, Some("a"), None);
        assert_eq!(
            waiting,
            vec![Waiting {
                session: "b".into(),
                state: AgentState::Done,
                count: 1,
            }]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_session_with_blocked_and_done_workspaces_shows_blocked_and_counts_both() {
        let _lock = crate::herdr::test_env_lock();
        let root = temp_root("m");
        let dir = root.join("sessions/b");
        fs::create_dir_all(&dir).unwrap();
        let snapshot = serde_json::json!({
            "type": "session_snapshot",
            "snapshot": {
                "workspaces": [
                    {"workspace_id": "w1", "label": "api", "focused": true},
                    {"workspace_id": "w2", "label": "web", "focused": false},
                ],
                "tabs": [
                    {"tab_id": "w1:t1", "workspace_id": "w1", "label": "1", "focused": true},
                    {"tab_id": "w2:t1", "workspace_id": "w2", "label": "1", "focused": true},
                ],
                "panes": [
                    {"pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1", "cwd": "/tmp", "focused": true},
                    {"pane_id": "w2:p1", "workspace_id": "w2", "tab_id": "w2:t1", "cwd": "/tmp", "focused": true},
                ],
                "agents": [
                    {"pane_id": "w1:p1", "agent": "claude", "agent_status": "done"},
                    {"pane_id": "w2:p1", "agent": "claude", "agent_status": "blocked"},
                ],
            }
        });
        let _server = serve(&dir, snapshot);

        assert_eq!(
            waiting(&root, Some("a"), None),
            vec![Waiting {
                session: "b".into(),
                state: AgentState::Blocked,
                count: 2,
            }]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_socket_that_never_answers_times_out_quickly() {
        let _lock = crate::herdr::test_env_lock();
        let root = temp_root("t");
        fs::create_dir_all(root.join("sessions/hung")).unwrap();
        let _listener = UnixListener::bind(root.join("sessions/hung/herdr.sock")).unwrap();

        let started = Instant::now();
        let topology = other_topology(&states(&root, None, None));
        assert!(topology.workspaces.is_empty());
        assert!(started.elapsed() < Duration::from_secs(1));

        let _ = fs::remove_dir_all(&root);
    }

    fn record(session: &str, client: &std::process::Child) -> LauncherRecord {
        LauncherRecord {
            launcher_pid: std::process::id(),
            client_pid: client.id(),
            session: session.into(),
        }
    }

    #[test]
    fn a_launcher_record_reads_back_while_its_launcher_and_herdr_client_live() {
        let _lock = crate::herdr::test_env_lock();
        let dir = temp_root("l");
        let client = fake_client(&dir);
        let mine = record("work", &client);
        write_record(&dir, &mine).unwrap();

        assert_eq!(launcher_at(&dir, "work"), Some(mine.clone()));
        assert_eq!(launcher_at(&dir, "other"), None);
        let stored: Value = read_json(&dir.join("work.json")).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({"launcher_pid": mine.launcher_pid, "client_pid": mine.client_pid, "session": "work"})
        );

        let newer = LauncherRecord {
            launcher_pid: mine.launcher_pid + 1,
            ..mine.clone()
        };
        write_record(&dir, &newer).unwrap();
        remove_record(&dir, &mine);
        assert!(
            dir.join("work.json").exists(),
            "another launcher's record stays"
        );
        write_record(&dir, &mine).unwrap();
        remove_record(&dir, &mine);
        assert!(!dir.join("work.json").exists());

        drop(client);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_is_stale_when_its_launcher_died_or_its_client_is_not_herdr() {
        let _lock = crate::herdr::test_env_lock();
        let dir = temp_root("x");
        let client = fake_client(&dir);

        let mut gone = sleeper();
        let gone_pid = gone.id();
        gone.kill().unwrap();
        gone.wait().unwrap();
        write_record(
            &dir,
            &LauncherRecord {
                launcher_pid: gone_pid,
                ..record("dead", &client)
            },
        )
        .unwrap();
        assert_eq!(launcher_at(&dir, "dead"), None);

        let plain = sleeper();
        write_record(&dir, &record("plain", &plain)).unwrap();
        assert_eq!(launcher_at(&dir, "plain"), None);

        let stranger = sleeper();
        write_record(
            &dir,
            &LauncherRecord {
                launcher_pid: stranger.id(),
                ..record("adopted", &client)
            },
        )
        .unwrap();
        assert_eq!(
            launcher_at(&dir, "adopted"),
            None,
            "client is not the launcher's child"
        );

        fs::write(dir.join("broken.json"), "{").unwrap();
        assert_eq!(launcher_at(&dir, "broken"), None);
        assert_eq!(launcher_at(&dir, "missing"), None);
        assert!(!process_alive(0));

        drop((client, plain, stranger));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_switch_request_ends_the_herdr_client_and_reads_back_once() {
        use std::os::unix::process::ExitStatusExt;

        let _lock = crate::herdr::test_env_lock();
        let dir = temp_root("q");
        let mut client = fake_client(&dir);
        let mine = record("a", &client);
        let target = SwitchRequest {
            session: "b".into(),
            workspace_id: Some("w2".into()),
            tab_id: None,
        };

        request_switch_at(&dir, &mine, &target).unwrap();
        assert_eq!(client.wait().unwrap().signal(), Some(15));
        let stored: Value = read_json(&switch_path(&dir, mine.launcher_pid)).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({"session": "b", "workspace_id": "w2"})
        );
        assert_eq!(
            take_switch_request(&dir, mine.launcher_pid),
            Some(target.clone())
        );
        assert_eq!(take_switch_request(&dir, mine.launcher_pid), None);

        let plain = sleeper();
        assert!(request_switch_at(&dir, &record("a", &plain), &target).is_err());
        assert!(
            process_alive(plain.id()),
            "a process that is not herdr keeps running"
        );
        assert!(!switch_path(&dir, mine.launcher_pid).exists());

        for session in ["../x", "a/b", "switch-1", ""] {
            let bad = SwitchRequest {
                session: session.into(),
                ..target.clone()
            };
            assert!(request_switch_at(&dir, &record("a", &plain), &bad).is_err());
        }
        write_json(
            &switch_path(&dir, mine.launcher_pid),
            &serde_json::json!({"session": "../x"}),
        )
        .unwrap();
        assert_eq!(take_switch_request(&dir, mine.launcher_pid), None);
        assert!(process_alive(plain.id()));

        drop(plain);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_session_socket_sits_in_the_root() {
        let root = Path::new("/h");
        assert_eq!(Session::at(root, "default").name, None);
        assert_eq!(socket_of(root, "default"), root.join("herdr.sock"));
        assert_eq!(socket_of(root, "b"), root.join("sessions/b/herdr.sock"));
    }
}
