use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{self, Command, ExitStatus},
    thread,
    time::{Duration, Instant},
};

use crate::{
    activity::{now_unix_secs, ActivityState},
    herdr::{herdr_bin, socket_request_at},
    paths::{activity_state_path, herdr_root},
    sessions::{self, LauncherRecord, SwitchRequest},
};

/// How long a switch waits for the target session to accept focus.
const FOCUS_DEADLINE: Duration = Duration::from_secs(5);
const FOCUS_RETRY: Duration = Duration::from_millis(100);

/// Shows the cursor and turns off mouse tracking, bracketed paste, focus
/// reports and the kitty keyboard mode. Each is harmless when already off.
const TERMINAL_RESET: &str =
    "\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[<u";

/// Leaves the alternate screen. Terminals also restore the cursor saved on
/// entry, which would write over a client's parting message, so this is sent
/// only before the next client or after a client that a signal ended.
const LEAVE_ALTERNATE_SCREEN: &str = "\x1b[?1049l";

fn last_session_path() -> PathBuf {
    sessions::launchers_dir().join("last-session")
}

/// The session to attach: the argument, else the one attached last, else `default`.
fn resolve_target(argument: Option<&str>, last: Option<&str>) -> Result<String, String> {
    let name = [argument, last]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
        .unwrap_or("default");
    sessions::check_session_name(name)?;
    Ok(name.to_string())
}

/// Runs `herdr session attach` until a client exits without a switch request,
/// then exits with that client's status.
pub(crate) fn run(argument: Option<&str>) -> ! {
    let dir = sessions::launchers_dir();
    let launcher_pid = process::id();
    sessions::clear_switch_request(&dir, launcher_pid);
    let last = fs::read_to_string(last_session_path()).ok();
    let session = resolve_target(argument, last.as_deref()).unwrap_or_else(|error| {
        eprintln!("helm-herdr shell: {error}");
        process::exit(2);
    });
    let mut target = SwitchRequest {
        session,
        workspace_id: None,
        tab_id: None,
    };
    loop {
        let status = attach(&dir, launcher_pid, &target);
        let next = sessions::take_switch_request(&dir, launcher_pid);
        let signalled = status.code().is_none();
        if status.success() || signalled || next.is_some() {
            let _ = sessions::write_atomic(&last_session_path(), target.session.as_bytes());
        }
        restore_terminal(next.is_some() || signalled);
        match next {
            Some(next) => target = next,
            None => process::exit(exit_code(status)),
        }
    }
}

/// One client: spawn, record, wait, drop the record.
fn attach(dir: &Path, launcher_pid: u32, target: &SwitchRequest) -> ExitStatus {
    let mut client = match Command::new(herdr_bin())
        .args(["session", "attach", &target.session])
        .spawn()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("failed to start herdr: {error}");
            process::exit(1);
        }
    };
    let record = LauncherRecord {
        launcher_pid,
        client_pid: client.id(),
        session: target.session.clone(),
    };
    let _ = sessions::write_record(dir, &record);
    let _ = record_seen(&activity_state_path(), &target.session, now_unix_secs());
    if let Some(workspace) = &target.workspace_id {
        focus_when_ready(
            sessions::socket_of(&herdr_root(), &target.session),
            workspace.clone(),
            target.tab_id.clone(),
        );
    }
    // The client reads Ctrl-C as a key; a stray SIGINT must not end the
    // launcher while its client still runs.
    sessions::ignore_interrupt(true);
    let status = client.wait();
    sessions::ignore_interrupt(false);
    let status = status.unwrap_or_else(|error| {
        eprintln!("failed to wait for herdr: {error}");
        process::exit(1);
    });
    let _ = record_seen(&activity_state_path(), &target.session, now_unix_secs());
    sessions::remove_record(dir, &record);
    status
}

/// Records that the User is in the session Herdr calls `session`.
fn record_seen(path: &Path, session: &str, now: i64) -> io::Result<()> {
    ActivityState::update_at(path, |activity| activity.record_seen(Some(session), now)).map(|_| ())
}

fn restore_terminal(leave_alternate_screen: bool) {
    let _ = Command::new("stty").arg("sane").status();
    let mut stdout = io::stdout();
    if leave_alternate_screen {
        let _ = stdout.write_all(LEAVE_ALTERNATE_SCREEN.as_bytes());
    }
    let _ = stdout.write_all(TERMINAL_RESET.as_bytes());
    let _ = stdout.flush();
}

/// Focuses `workspace` (and `tab`) through `socket` once the session answers,
/// retrying in the background until [`FOCUS_DEADLINE`].
fn focus_when_ready(socket: PathBuf, workspace: String, tab: Option<String>) {
    thread::spawn(move || {
        let deadline = Instant::now() + FOCUS_DEADLINE;
        while Instant::now() < deadline {
            if focus(&socket, &workspace, tab.as_deref()).is_ok() {
                return;
            }
            thread::sleep(FOCUS_RETRY);
        }
    });
}

fn focus(socket: &Path, workspace: &str, tab: Option<&str>) -> Result<(), String> {
    let request = |method: &str, params| {
        socket_request_at(
            socket,
            &format!("helm-herdr-shell-{}", process::id()),
            method,
            params,
            Some(Duration::from_millis(300)),
        )
    };
    request(
        "workspace.focus",
        serde_json::json!({ "workspace_id": workspace }),
    )?;
    if let Some(tab) = tab {
        request("tab.focus", serde_json::json!({ "tab_id": tab }))?;
    }
    Ok(())
}

fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_argument_wins_then_the_last_session_then_default() {
        assert_eq!(resolve_target(Some("work"), Some("b")).unwrap(), "work");
        assert_eq!(resolve_target(None, Some("b\n")).unwrap(), "b");
        assert_eq!(resolve_target(None, Some("  ")).unwrap(), "default");
        assert_eq!(resolve_target(None, None).unwrap(), "default");
        assert_eq!(resolve_target(Some(""), Some("b")).unwrap(), "b");
    }

    #[test]
    fn attaching_and_leaving_record_the_session_as_seen() {
        let dir = std::env::temp_dir().join(format!("helm-launcher-seen-{}", process::id()));
        let path = dir.join("activity.json");
        record_seen(&path, "work", 10).unwrap();
        record_seen(&path, "default", 20).unwrap();
        let activity = ActivityState::load_from(&path).unwrap();
        assert_eq!(activity.seen_at(Some("work")), Some(10));
        assert_eq!(activity.seen_at(None), Some(20));

        record_seen(&path, "work", 30).unwrap();
        let activity = ActivityState::load_from(&path).unwrap();
        assert_eq!(
            activity.seen_at(Some("work")),
            Some(30),
            "the exit time wins"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_that_leave_the_launchers_directory_are_refused() {
        for name in ["../x", "a/b", "..", "switch-42"] {
            assert!(resolve_target(Some(name), None).is_err(), "{name}");
        }
        assert!(resolve_target(None, Some("../x")).is_err());
    }
}
