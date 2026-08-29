use std::{
    env,
    process::{self, Command},
};

mod app;
mod config;
mod herdr;
mod integrations;
mod keymap;
mod matcher;
mod model;
mod paths;
mod recent;
mod sources;
mod theme;
mod topology;
mod tui;
mod update;

use app::App;
use config::{Config, Percentage};
use herdr::herdr_bin;
use theme::Theme;
use tui::tui_loop;

fn main() {
    match env::args().nth(1).as_deref() {
        Some("open") => open_picker(),
        Some("open-side") => open_side_picker(),
        Some("jump-back") => jump_back(),
        Some("ui") => run_ui(env::args().nth(2).as_deref() == Some("--side")),
        Some("list") => debug_list(),
        _ => {
            eprintln!("usage: helm-herdr <open|open-side|jump-back|ui|list>");
            process::exit(2);
        }
    }
}

const SIDE_PANE_LABEL: &str = "Helm Side";

enum SideDecision {
    Open,
    Focus(String),
    Close(String),
}

fn pane_in_focused_workspace<'a>(
    pane_json: &'a serde_json::Value,
    label: &str,
) -> Option<(&'a serde_json::Value, &'a serde_json::Value)> {
    let panes = pane_json
        .pointer("/result/panes")
        .and_then(|v| v.as_array())?;
    let focused = panes
        .iter()
        .find(|p| p.get("focused").and_then(|v| v.as_bool()) == Some(true))?;
    let workspace = focused.get("workspace_id").and_then(|v| v.as_str())?;
    let target = panes.iter().find(|p| {
        p.get("label").and_then(|v| v.as_str()) == Some(label)
            && p.get("workspace_id").and_then(|v| v.as_str()) == Some(workspace)
    })?;
    Some((focused, target))
}

fn popup_request(
    plugin_id: &str,
    entrypoint: &str,
    width: Percentage,
    height: Percentage,
    request_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": request_id,
        "method": "plugin.pane.open",
        "params": {
            "plugin_id": plugin_id,
            "entrypoint": entrypoint,
            "placement": "popup",
            "width": width.to_string(),
            "height": height.to_string(),
        }
    })
}

fn open_picker() -> ! {
    let config = Config::load();
    let plugin = env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "helm-herdr".into());
    open_plugin_popup(
        &plugin,
        "picker",
        config.picker.popup_width,
        config.picker.popup_height,
    )
}

// Launch-or-focus, toggle on repeat — same UX as herdr-file-viewer's side pane,
// scoped to the focused workspace. Any parse failure degrades to Open.
fn side_pane_decision(pane_json: &serde_json::Value) -> SideDecision {
    let Some((focused, side)) = pane_in_focused_workspace(pane_json, SIDE_PANE_LABEL) else {
        return SideDecision::Open;
    };
    let Some(id) = side.get("pane_id").and_then(|v| v.as_str()) else {
        return SideDecision::Open;
    };
    if focused.get("pane_id").and_then(|v| v.as_str()) == Some(id) {
        SideDecision::Close(id.into())
    } else {
        SideDecision::Focus(id.into())
    }
}

fn open_side_picker() -> ! {
    let json = herdr::herdr_json(["pane", "list"]).unwrap_or(serde_json::Value::Null);
    match side_pane_decision(&json) {
        SideDecision::Open => open_plugin_pane(
            "picker-side",
            &["--placement", "split", "--direction", "right", "--focus"],
        ),
        SideDecision::Focus(id) => run_plugin_pane_cmd("focus", &id),
        SideDecision::Close(id) => run_plugin_pane_cmd("close", &id),
    }
}

fn open_plugin_popup(
    plugin_id: &str,
    entrypoint: &str,
    width: Percentage,
    height: Percentage,
) -> ! {
    let request = popup_request(plugin_id, entrypoint, width, height, "helm-herdr-popup");
    match herdr::socket_request_with_id(
        request["id"].as_str().unwrap_or_default(),
        request["method"].as_str().unwrap_or_default(),
        request["params"].clone(),
    ) {
        Ok(_) => process::exit(0),
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}

fn open_plugin_pane(entrypoint: &str, extra: &[&str]) -> ! {
    let plugin = env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "helm-herdr".into());
    let status = Command::new(herdr_bin())
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            &plugin,
            "--entrypoint",
            entrypoint,
        ])
        .args(extra)
        .status();
    match status {
        Ok(s) => process::exit(s.code().unwrap_or(0)),
        Err(error) => {
            eprintln!("failed to open picker pane: {error}");
            process::exit(1);
        }
    }
}

fn run_plugin_pane_cmd(cmd: &str, pane_id: &str) -> ! {
    let status = plugin_pane_command(cmd, pane_id);
    process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
}

fn plugin_pane_command(cmd: &str, pane_id: &str) -> std::io::Result<process::ExitStatus> {
    Command::new(herdr_bin())
        .args(["plugin", "pane", cmd, pane_id])
        .status()
}

fn jump_back() -> ! {
    let config = Config::load();
    match app::jump_back(&config) {
        Ok(label) => {
            herdr::notify_done(&format!("Jumped back to {label}"), &config.notifications);
            process::exit(0);
        }
        Err(error) => {
            herdr::notify_error(&format!("Jump back failed: {error}"), &config.notifications);
            eprintln!("jump back failed: {error}");
            process::exit(1);
        }
    }
}

fn run_ui(persist: bool) -> ! {
    let config = Config::load();
    let check_updates = config.picker.check_updates;
    let mut app = App::new(config, Theme::terminal());
    app.refresh();
    let update_check = check_updates.then(update::check_in_background);

    if let Err(e) = tui_loop(&mut app, persist, update_check) {
        eprintln!("Navigator error: {e}");
        process::exit(1);
    }
    process::exit(0);
}

fn debug_list() {
    let mut app = App::new(Config::load(), Theme::terminal());
    app.refresh();
    for e in app.entries {
        println!("{}\t{}\t{}", e.source_name(), e.title, e.path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, ws: &str, label: Option<&str>, focused: bool) -> serde_json::Value {
        let mut p = serde_json::json!({"pane_id": id, "workspace_id": ws, "focused": focused});
        if let Some(label) = label {
            p["label"] = label.into();
        }
        p
    }

    fn pane_list(panes: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({"id": "cli:pane:list", "result": {"type": "pane_list", "panes": panes}})
    }

    #[test]
    fn popup_request_uses_validated_percentage_dimensions() {
        let request = popup_request(
            "helm-herdr",
            "picker",
            config::Percentage::try_from(90).unwrap(),
            config::Percentage::try_from(80).unwrap(),
            "request-1",
        );
        assert_eq!(
            request,
            serde_json::json!({
                "id": "request-1",
                "method": "plugin.pane.open",
                "params": {
                    "plugin_id": "helm-herdr",
                    "entrypoint": "picker",
                    "placement": "popup",
                    "width": "90%",
                    "height": "80%"
                }
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn side_pane_focus_uses_cli_wrapper() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        let _lock = herdr::test_env_lock();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("helm-herdr-side-cli-{stamp}"));
        fs::create_dir(&dir).unwrap();
        let script = dir.join("herdr");
        let args = dir.join("args");
        fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HERDR_TEST_ARGS\"\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let previous_bin = env::var_os("HERDR_BIN_PATH");
        let previous_args = env::var_os("HERDR_TEST_ARGS");
        env::set_var("HERDR_BIN_PATH", &script);
        env::set_var("HERDR_TEST_ARGS", &args);
        let status = plugin_pane_command("focus", "w1:p2").unwrap();
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(&args).unwrap(),
            "plugin\npane\nfocus\nw1:p2\n"
        );

        match previous_bin {
            Some(value) => env::set_var("HERDR_BIN_PATH", value),
            None => env::remove_var("HERDR_BIN_PATH"),
        }
        match previous_args {
            Some(value) => env::set_var("HERDR_TEST_ARGS", value),
            None => env::remove_var("HERDR_TEST_ARGS"),
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn side_pane_toggles_open_focus_close() {
        let no_side = pane_list(vec![pane("w1:p1", "w1", None, true)]);
        assert!(matches!(side_pane_decision(&no_side), SideDecision::Open));

        let unfocused_side = pane_list(vec![
            pane("w1:p1", "w1", None, true),
            pane("w1:p2", "w1", Some(SIDE_PANE_LABEL), false),
        ]);
        assert!(
            matches!(side_pane_decision(&unfocused_side), SideDecision::Focus(id) if id == "w1:p2")
        );

        let focused_side = pane_list(vec![
            pane("w1:p1", "w1", None, false),
            pane("w1:p2", "w1", Some(SIDE_PANE_LABEL), true),
        ]);
        assert!(
            matches!(side_pane_decision(&focused_side), SideDecision::Close(id) if id == "w1:p2")
        );

        let other_workspace = pane_list(vec![
            pane("w1:p1", "w1", None, true),
            pane("w2:p2", "w2", Some(SIDE_PANE_LABEL), false),
        ]);
        assert!(matches!(
            side_pane_decision(&other_workspace),
            SideDecision::Open
        ));

        assert!(matches!(
            side_pane_decision(&serde_json::Value::Null),
            SideDecision::Open
        ));
    }
}
