use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

/// Herdr's config directory, which holds the default session and `sessions/`.
pub(crate) fn herdr_root() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(value).join("herdr"))
        .unwrap_or_else(|| home().join(".config/herdr"))
}

pub(crate) fn plugin_config_dir() -> PathBuf {
    env::var("HERDR_PLUGIN_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| herdr_root().join("plugins/config/helm-herdr"))
}

pub(crate) fn migrate_legacy_plugin_config() {
    let current = plugin_config_dir();
    let legacy_picker_plus = herdr_root().join("plugins/config/herdr-picker-plus");
    let legacy_navigator = herdr_root().join("plugins/config/herdr-navigator");
    if current != legacy_picker_plus {
        let _ = copy_missing_files(&legacy_picker_plus, &current);
    }
    if current != legacy_navigator {
        let _ = copy_missing_files(&legacy_navigator, &current);
    }
}

fn copy_missing_files(from: &Path, to: &Path) -> io::Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let destination = to.join(entry.file_name());
        if entry.file_type()?.is_file() && !destination.exists() {
            fs::copy(entry.path(), destination)?;
        }
    }
    Ok(())
}
pub(crate) fn recent_workspaces_state_path() -> PathBuf {
    plugin_config_dir().join("recent-workspaces.json")
}

pub(crate) fn activity_state_path() -> PathBuf {
    plugin_config_dir().join("activity.json")
}

pub(crate) fn herdr_plus_projects_dir() -> PathBuf {
    home().join(".config/herdr/plugins/config/cloudmanic.herdr-plus/projects")
}
pub(crate) fn herdr_plus_quick_actions_dir() -> PathBuf {
    home().join(".config/herdr/plugins/config/cloudmanic.herdr-plus/quick-actions")
}
pub(crate) fn home() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}
pub(crate) fn expand_path(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        home().join(rest)
    } else if s == "~" {
        home()
    } else {
        PathBuf::from(s.replace("$HOME", &home().display().to_string()))
    }
}
pub(crate) fn basename(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace")
        .to_string()
}
pub(crate) fn canonical_str(path: &Path) -> Option<String> {
    fs::canonicalize(path).ok().map(|p| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn recent_workspaces_state_uses_plugin_config_directory() {
        assert_eq!(
            recent_workspaces_state_path().file_name().unwrap(),
            "recent-workspaces.json"
        );
        assert_eq!(
            recent_workspaces_state_path().parent().unwrap(),
            plugin_config_dir()
        );
    }

    #[test]
    fn herdr_root_follows_xdg_config_home_and_falls_back_to_home() {
        struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                for (name, value) in &self.0 {
                    match value {
                        Some(value) => env::set_var(name, value),
                        None => env::remove_var(name),
                    }
                }
            }
        }
        let _lock = crate::herdr::test_env_lock();
        let _restore = RestoreEnv(
            ["XDG_CONFIG_HOME", "HOME", "HERDR_PLUGIN_CONFIG_DIR"]
                .into_iter()
                .map(|name| (name, env::var_os(name)))
                .collect(),
        );
        let temp = env::temp_dir().join(format!("helm-root-{}", std::process::id()));
        env::remove_var("HERDR_PLUGIN_CONFIG_DIR");
        env::set_var("HOME", temp.join("home"));

        env::set_var("XDG_CONFIG_HOME", temp.join("xdg"));
        assert_eq!(herdr_root(), temp.join("xdg/herdr"));
        assert_eq!(
            plugin_config_dir(),
            temp.join("xdg/herdr/plugins/config/helm-herdr")
        );

        env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(herdr_root(), temp.join("home/.config/herdr"));
        assert_eq!(
            plugin_config_dir(),
            temp.join("home/.config/herdr/plugins/config/helm-herdr")
        );
    }

    #[test]
    fn legacy_config_files_copy_without_overwriting_new_values() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let old = env::temp_dir().join(format!("herdr-navigator-old-{suffix}"));
        let new = env::temp_dir().join(format!("herdr-navigator-new-{suffix}"));
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("config.toml"), "old").unwrap();
        fs::write(old.join("jump-back-workspace"), "w1").unwrap();
        fs::write(new.join("config.toml"), "new").unwrap();

        copy_missing_files(&old, &new).unwrap();

        assert_eq!(fs::read_to_string(new.join("config.toml")).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(new.join("jump-back-workspace")).unwrap(),
            "w1"
        );
        let _ = fs::remove_dir_all(old);
        let _ = fs::remove_dir_all(new);
    }
}
