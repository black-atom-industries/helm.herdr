use std::{
    env,
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    thread,
};

#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::PathBuf};

#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

#[cfg(test)]
static TEST_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn test_env_lock() -> MutexGuard<'static, ()> {
    TEST_ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

use serde_json::Value;

use crate::{config::NotificationsConfig, paths::expand_path};

pub(crate) fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into())
}

#[cfg(unix)]
pub(crate) fn socket_request(method: &str, params: Value) -> Result<Value, String> {
    socket_request_with_id(
        &format!("helm-herdr-{}", std::process::id()),
        method,
        params,
    )
}

#[cfg(unix)]
pub(crate) fn socket_request_with_id(
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let path = env::var_os("HERDR_SOCKET_PATH")
        .map(PathBuf::from)
        .ok_or_else(|| "HERDR_SOCKET_PATH is not set".to_string())?;
    let request = serde_json::json!({
        "id": id,
        "method": method,
        "params": params,
    });
    let mut stream = UnixStream::connect(&path).map_err(|error| {
        format!(
            "failed to connect to Herdr socket {}: {error}",
            path.display()
        )
    })?;
    let line = serde_json::to_string(&request)
        .map_err(|error| format!("failed to encode Herdr request: {error}"))?;
    stream
        .write_all(line.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .map_err(|error| format!("failed to write Herdr request: {error}"))?;
    let mut response_line = String::new();
    let bytes = BufReader::new(stream)
        .read_line(&mut response_line)
        .map_err(|error| format!("failed to read Herdr response: {error}"))?;
    if bytes == 0 {
        return Err("Herdr socket returned EOF".into());
    }
    let response: Value = serde_json::from_str(response_line.trim_end())
        .map_err(|error| format!("malformed Herdr response: {error}"))?;
    if response.get("id").and_then(Value::as_str) != Some(id) {
        return Err("Herdr response ID did not match request".into());
    }
    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(Value::as_str);
        let message = error.get("message").and_then(Value::as_str);
        return match (code, message) {
            (Some(code), Some(message)) => Err(format!("Herdr API error ({code}): {message}")),
            _ => Err("malformed Herdr API error response".into()),
        };
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "malformed Herdr success response".into())
}

#[cfg(not(unix))]
pub(crate) fn socket_request(_method: &str, _params: Value) -> Result<Value, String> {
    Err("Herdr socket API requires Unix sockets".into())
}

#[cfg(not(unix))]
pub(crate) fn socket_request_with_id(
    _id: &str,
    _method: &str,
    _params: Value,
) -> Result<Value, String> {
    Err("Herdr socket API requires Unix sockets".into())
}

pub(crate) fn focus_pane(pane_id: &str) -> Result<(), String> {
    socket_request("pane.focus", serde_json::json!({ "pane_id": pane_id })).map(|_| ())
}
pub(crate) fn herdr_json<const N: usize>(args: [&str; N]) -> Result<Value, String> {
    herdr_json_args(args)
}

pub(crate) fn herdr_json_args<I, S>(args: I) -> Result<Value, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let out = Command::new(herdr_bin())
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

pub(crate) fn run_herdr<const N: usize>(args: [&str; N]) -> Result<(), String> {
    run_herdr_args(args)
}

pub(crate) fn run_herdr_args<I, S>(args: I) -> Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let status = Command::new(herdr_bin())
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("herdr exited with {status}"))
    }
}

pub(crate) fn notify_done(body: &str, config: &NotificationsConfig) {
    notify(body, "done", config);
}

pub(crate) fn notify_error(body: &str, config: &NotificationsConfig) {
    notify(body, "request", config);
}

fn notify(body: &str, default_sound: &'static str, config: &NotificationsConfig) {
    if !config.enabled {
        return;
    }
    let body = truncate(body, 180);
    let (sound, custom_sound) = notification_audio(config, default_sound);
    let _ = Command::new(herdr_bin())
        .args([
            "notification",
            "show",
            "Helm",
            "--body",
            &body,
            "--position",
            "top-right",
            "--sound",
            sound,
        ])
        .status();
    if let Some(path) = custom_sound {
        play_custom_sound(path);
    }
}

fn notification_audio<'a>(
    config: &'a NotificationsConfig,
    default_sound: &'static str,
) -> (&'static str, Option<&'a str>) {
    if !config.audio {
        return ("none", None);
    }
    match config.sound.trim().to_ascii_lowercase().as_str() {
        "default" => (default_sound, None),
        "custom" => (
            "none",
            config
                .custom_sound
                .as_deref()
                .filter(|path| !path.is_empty()),
        ),
        _ => ("none", None),
    }
}

fn play_custom_sound(path: &str) {
    let path = expand_path(path);
    if !path.is_file() {
        return;
    }
    thread::spawn(move || {
        #[cfg(target_os = "macos")]
        let players: &[(&str, &[&str])] = &[("afplay", &[])];
        #[cfg(not(target_os = "macos"))]
        let players: &[(&str, &[&str])] = &[("pw-play", &[]), ("paplay", &[]), ("aplay", &[])];

        for (player, args) in players {
            let status = Command::new(player)
                .args(*args)
                .arg(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if status.is_ok_and(|status| status.success()) {
                break;
            }
        }
    });
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut out: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotificationsConfig;

    #[cfg(unix)]
    fn socket_fixture(
        response: impl FnOnce(String) -> String + Send + 'static,
    ) -> (std::path::PathBuf, std::thread::JoinHandle<()>) {
        use std::{
            os::unix::net::UnixListener,
            time::{SystemTime, UNIX_EPOCH},
        };
        let path = std::env::temp_dir().join(format!(
            "helm-herdr-test-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            let mut byte = [0u8; 1];
            loop {
                let count = std::io::Read::read(&mut stream, &mut byte).unwrap();
                if count == 0 {
                    break;
                }
                line.push(byte[0] as char);
                if byte[0] == b'\n' {
                    break;
                }
            }
            writeln!(stream, "{}", response(line)).unwrap();
        });
        (path, handle)
    }

    #[cfg(unix)]
    fn eof_socket_fixture() -> (std::path::PathBuf, std::thread::JoinHandle<()>) {
        use std::{
            os::unix::net::UnixListener,
            time::{SystemTime, UNIX_EPOCH},
        };
        let path = std::env::temp_dir().join(format!(
            "helm-herdr-eof-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            let _ = BufReader::new(stream).read_line(&mut line);
        });
        (path, handle)
    }

    #[cfg(unix)]
    #[test]
    fn socket_requests_are_newline_json_and_require_matching_success_id() {
        let _lock = test_env_lock();
        let (path, handle) = socket_fixture(|line| {
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "pane.focus");
            assert_eq!(request["params"], serde_json::json!({"pane_id": "w1:p1"}));
            serde_json::json!({"id": request["id"], "result": {"type": "ok"}}).to_string()
        });
        let previous = env::var_os("HERDR_SOCKET_PATH");
        env::set_var("HERDR_SOCKET_PATH", &path);
        assert_eq!(focus_pane("w1:p1"), Ok(()));
        if let Some(value) = previous {
            env::set_var("HERDR_SOCKET_PATH", value);
        } else {
            env::remove_var("HERDR_SOCKET_PATH");
        }
        handle.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn socket_errors_preserve_api_code_and_reject_malformed_eof_and_missing_socket() {
        let _lock = test_env_lock();
        let (path, handle) = socket_fixture(|line| {
            let request: Value = serde_json::from_str(&line).unwrap();
            serde_json::json!({"id": request["id"], "error": {"code": "ui_busy", "message": "busy"}}).to_string()
        });
        let previous = env::var_os("HERDR_SOCKET_PATH");
        env::set_var("HERDR_SOCKET_PATH", &path);
        let error = socket_request("plugin.pane.open", serde_json::json!({})).unwrap_err();
        assert_eq!(error, "Herdr API error (ui_busy): busy");
        if let Some(value) = previous {
            env::set_var("HERDR_SOCKET_PATH", value);
        } else {
            env::remove_var("HERDR_SOCKET_PATH");
        }
        handle.join().unwrap();

        let missing = env::temp_dir().join(format!("helm-herdr-missing-{}", std::process::id()));
        env::set_var("HERDR_SOCKET_PATH", &missing);
        assert!(socket_request("pane.focus", serde_json::json!({}))
            .unwrap_err()
            .contains("failed to connect"));
        env::remove_var("HERDR_SOCKET_PATH");

        let (path, handle) = socket_fixture(|_| "not json".into());
        env::set_var("HERDR_SOCKET_PATH", &path);
        assert!(socket_request("pane.focus", serde_json::json!({}))
            .unwrap_err()
            .contains("malformed Herdr response"));
        handle.join().unwrap();

        let (path, handle) =
            socket_fixture(|_| serde_json::json!({"id": "wrong", "result": {}}).to_string());
        env::set_var("HERDR_SOCKET_PATH", &path);
        assert_eq!(
            socket_request("pane.focus", serde_json::json!({})).unwrap_err(),
            "Herdr response ID did not match request"
        );
        handle.join().unwrap();

        let (path, handle) = eof_socket_fixture();
        env::set_var("HERDR_SOCKET_PATH", &path);
        assert_eq!(
            socket_request("pane.focus", serde_json::json!({})).unwrap_err(),
            "Herdr socket returned EOF"
        );
        handle.join().unwrap();
        env::remove_var("HERDR_SOCKET_PATH");
    }

    #[test]
    fn notification_audio_resolves_silent_default_herdr_and_custom() {
        let silent = NotificationsConfig::default();
        assert_eq!(notification_audio(&silent, "request"), ("none", None));

        let herdr = NotificationsConfig {
            enabled: true,
            audio: true,
            sound: "default".into(),
            custom_sound: None,
        };
        assert_eq!(notification_audio(&herdr, "done"), ("done", None));

        let custom = NotificationsConfig {
            enabled: true,
            audio: true,
            sound: "custom".into(),
            custom_sound: Some("~/sounds/navigator.wav".into()),
        };
        assert_eq!(
            notification_audio(&custom, "done"),
            ("none", Some("~/sounds/navigator.wav"))
        );
    }
}
