use std::{collections::HashMap, fs};

use serde::Deserialize;

use crate::{
    glance::IconStyle,
    model::Source,
    paths::{migrate_legacy_plugin_config, plugin_config_dir},
};

const DEFAULT_CONFIG: &str = include_str!("../examples/default-config.toml");

#[derive(Clone, Deserialize)]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) picker: PickerConfig,
    #[serde(default)]
    pub(crate) jump_back: JumpBackConfig,
    #[serde(default)]
    pub(crate) glance: GlanceConfig,
    #[serde(default)]
    pub(crate) notifications: NotificationsConfig,
    #[serde(default)]
    pub(crate) sources: SourcesConfig,
    #[serde(default)]
    pub(crate) theme: ThemeConfig,
    #[serde(default)]
    pub(crate) roots: Vec<RootConfig>,
    #[serde(default)]
    pub(crate) integrations: Vec<IntegrationConfig>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct PickerConfig {
    #[serde(default = "yes")]
    pub(crate) reuse_existing: bool,
    #[serde(default = "yes")]
    pub(crate) create_missing: bool,
    #[serde(default = "default_engine")]
    pub(crate) engine: String,
    #[serde(default = "default_source_order")]
    pub(crate) source_order: Vec<String>,
    #[serde(default = "default_source_priority_boost")]
    pub(crate) source_priority_boost: i64,
    #[serde(default = "yes")]
    pub(crate) check_updates: bool,
    #[serde(default)]
    pub(crate) directory_template: Option<String>,
    #[serde(default = "default_directory_template_key")]
    pub(crate) directory_template_key: String,
    #[serde(default)]
    pub(crate) filter_keys: HashMap<String, String>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct JumpBackConfig {
    #[serde(default = "yes")]
    pub(crate) enabled: bool,
}

#[derive(Clone, Deserialize)]
pub(crate) struct GlanceConfig {
    #[serde(default = "default_active_minutes")]
    pub(crate) active_minutes: i64,
    #[serde(default = "default_recent_hours")]
    pub(crate) recent_hours: i64,
    #[serde(default)]
    pub(crate) icons: IconStyle,
}

#[derive(Clone, Deserialize)]
pub(crate) struct NotificationsConfig {
    #[serde(default = "yes")]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) audio: bool,
    #[serde(default = "default_notification_sound")]
    pub(crate) sound: String,
    #[serde(default)]
    pub(crate) custom_sound: Option<String>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct SourcesConfig {
    #[serde(default = "yes")]
    pub(crate) open_workspaces: bool,
    #[serde(default = "yes")]
    pub(crate) zoxide: bool,
    #[serde(default = "yes")]
    pub(crate) roots: bool,
    #[serde(default = "yes")]
    pub(crate) herdr_plus_quick_actions: bool,
}

impl SourcesConfig {
    pub(crate) fn enabled(&self, source: &Source) -> bool {
        match source {
            Source::Workspace => self.open_workspaces,
            Source::Zoxide => self.zoxide,
            Source::Root => self.roots,
            Source::QuickAction => self.herdr_plus_quick_actions,
            Source::Integration => true,
        }
    }
}

#[derive(Clone, Deserialize)]
pub(crate) struct IntegrationConfig {
    pub(crate) id: String,
    pub(crate) label: String,
    #[serde(default = "yes")]
    pub(crate) enabled: bool,
    pub(crate) collect: String,
    pub(crate) open: String,
    #[serde(default = "yes")]
    pub(crate) notify_success: bool,
    #[serde(default = "yes")]
    pub(crate) notify_error: bool,
}

#[derive(Clone, Deserialize)]
pub(crate) struct ThemeConfig {
    #[serde(default = "yes")]
    pub(crate) inherit_herdr: bool,
}

#[derive(Clone, Deserialize)]
pub(crate) struct RootConfig {
    pub(crate) path: String,
    #[serde(default = "default_depth")]
    pub(crate) max_depth: usize,
}
fn yes() -> bool {
    true
}
fn default_depth() -> usize {
    3
}
fn default_engine() -> String {
    "nucleo".into()
}
fn default_source_order() -> Vec<String> {
    ["workspace", "zoxide", "root", "quick", "plugin"]
        .into_iter()
        .map(String::from)
        .collect()
}
fn default_source_priority_boost() -> i64 {
    5
}
fn default_directory_template_key() -> String {
    "alt-enter".into()
}
fn default_active_minutes() -> i64 {
    15
}
fn default_recent_hours() -> i64 {
    4
}
fn default_notification_sound() -> String {
    "default".into()
}
fn default_filter_key(source: &Source) -> Option<char> {
    match source {
        Source::QuickAction => Some('q'),
        Source::Workspace => Some('w'),
        Source::Zoxide => Some('z'),
        Source::Root => Some('r'),
        Source::Integration => None,
    }
}

fn default_filter_keys() -> Vec<(Source, char)> {
    Source::all()
        .into_iter()
        .filter_map(|source| default_filter_key(&source).map(|key| (source, key)))
        .collect()
}

pub(crate) fn parse_ctrl_key(value: &str) -> Option<char> {
    let key = value
        .trim()
        .to_ascii_lowercase()
        .replace("ctrl+", "")
        .replace("ctrl-", "")
        .replace(['^', '⌃'], "");
    let mut chars = key.chars();
    let ch = chars.next()?;
    (chars.next().is_none() && ch.is_ascii_alphanumeric()).then_some(ch)
}

impl Default for PickerConfig {
    fn default() -> Self {
        Self {
            reuse_existing: true,
            create_missing: true,
            engine: default_engine(),
            source_order: default_source_order(),
            source_priority_boost: default_source_priority_boost(),
            check_updates: true,
            directory_template: None,
            directory_template_key: default_directory_template_key(),
            filter_keys: HashMap::new(),
        }
    }
}

impl PickerConfig {
    pub(crate) fn filter_key(&self, source: &Source) -> Option<char> {
        let key = self
            .custom_filter_keys()
            .into_iter()
            .find_map(|(custom_source, key)| (custom_source == *source).then_some(key))
            .or_else(|| default_filter_key(source))?;
        (self.filter_source_for_key(key).as_ref() == Some(source)).then_some(key)
    }

    pub(crate) fn filter_source_for_key(&self, key: char) -> Option<Source> {
        let key = key.to_ascii_lowercase();
        let custom = self.custom_filter_keys();
        custom
            .iter()
            .find(|(_, custom_key)| *custom_key == key)
            .map(|(source, _)| source.clone())
            .or_else(|| {
                default_filter_keys()
                    .into_iter()
                    .find_map(|(source, default_key)| {
                        (default_key == key && !custom.iter().any(|(s, _)| s == &source))
                            .then_some(source)
                    })
            })
    }

    fn custom_filter_keys(&self) -> Vec<(Source, char)> {
        self.filter_keys
            .iter()
            .filter_map(|(source, key)| Some((Source::from_config(source)?, parse_ctrl_key(key)?)))
            .collect()
    }

    pub(crate) fn source_rank(&self, source: &Source) -> usize {
        self.source_order
            .iter()
            .filter_map(|name| Source::from_config(name))
            .position(|item| &item == source)
            .unwrap_or_else(|| Source::all().len())
    }

    pub(crate) fn source_bonus(&self, source: &Source) -> i64 {
        let rank = self.source_rank(source) as i64;
        let total = Source::all().len() as i64;
        (total - rank).max(0) * self.source_priority_boost
    }
}
impl Default for JumpBackConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}
impl Default for GlanceConfig {
    fn default() -> Self {
        Self {
            active_minutes: default_active_minutes(),
            recent_hours: default_recent_hours(),
            icons: IconStyle::default(),
        }
    }
}
impl Default for SourcesConfig {
    fn default() -> Self {
        Self {
            open_workspaces: true,
            zoxide: true,
            roots: true,
            herdr_plus_quick_actions: true,
        }
    }
}
impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            audio: false,
            sound: default_notification_sound(),
            custom_sound: None,
        }
    }
}
impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            inherit_herdr: true,
        }
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            picker: PickerConfig::default(),
            jump_back: JumpBackConfig::default(),
            glance: GlanceConfig::default(),
            notifications: NotificationsConfig::default(),
            sources: SourcesConfig::default(),
            theme: ThemeConfig::default(),
            integrations: vec![],
            roots: vec![
                RootConfig {
                    path: "~/workspace".into(),
                    max_depth: 3,
                },
                RootConfig {
                    path: "~/work".into(),
                    max_depth: 3,
                },
                RootConfig {
                    path: "~/projects".into(),
                    max_depth: 3,
                },
            ],
        }
    }
}

impl Config {
    pub(crate) fn enabled_sources_in_order(&self) -> Vec<Source> {
        let mut sources: Vec<_> = Source::all()
            .into_iter()
            .filter(|source| self.sources.enabled(source))
            .collect();
        sources.sort_by_key(|source| self.picker.source_rank(source));
        sources
    }

    pub(crate) fn load() -> Self {
        migrate_legacy_plugin_config();
        let dir = plugin_config_dir();
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        if !path.exists() {
            let _ = fs::write(&path, DEFAULT_CONFIG);
        }
        fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_source_order_prioritizes_open_workspaces() {
        let picker = PickerConfig::default();

        assert_eq!(picker.source_rank(&Source::Workspace), 0);
        assert_eq!(picker.source_rank(&Source::Zoxide), 1);
        assert_eq!(picker.source_rank(&Source::Root), 2);
        assert!(picker.source_bonus(&Source::Root) > picker.source_bonus(&Source::Integration));
    }

    #[test]
    fn enabled_sources_follow_configured_order() {
        let config: Config = toml::from_str(
            r#"
            [picker]
            source_order = ["root", "workspace"]

            [sources]
            servers = false
            sessions = false
            "#,
        )
        .unwrap();

        let sources = config.enabled_sources_in_order();
        assert_eq!(sources[0], Source::Root);
        assert_eq!(sources[1], Source::Workspace);
    }

    #[test]
    fn directory_template_defaults_off_and_accepts_herdr_plus_filename() {
        assert!(Config::default().picker.directory_template.is_none());
        assert_eq!(Config::default().picker.directory_template_key, "alt-enter");

        let config: Config = toml::from_str(
            r#"
            [picker]
            directory_template = "default.toml"
            directory_template_key = "ctrl-g"
            "#,
        )
        .unwrap();

        assert_eq!(
            config.picker.directory_template.as_deref(),
            Some("default.toml")
        );
        assert_eq!(config.picker.directory_template_key, "ctrl-g");
    }

    #[test]
    fn update_checks_default_on_and_can_be_disabled() {
        assert!(Config::default().picker.check_updates);

        let config: Config = toml::from_str(
            r#"
            [picker]
            check_updates = false
            "#,
        )
        .unwrap();

        assert!(!config.picker.check_updates);
    }

    #[test]
    fn notification_audio_defaults_off_and_supports_custom() {
        let default = Config::default().notifications;
        assert!(!default.audio);
        assert_eq!(default.sound, "default");

        let custom: Config = toml::from_str(
            r#"
            [notifications]
            audio = true
            sound = "custom"
            custom_sound = "~/sounds/navigator.wav"
            "#,
        )
        .unwrap();
        assert!(custom.notifications.audio);
        assert_eq!(custom.notifications.sound, "custom");
        assert_eq!(
            custom.notifications.custom_sound.as_deref(),
            Some("~/sounds/navigator.wav")
        );

        let silent: Config = toml::from_str(
            r#"
            [notifications]
            audio = false
            "#,
        )
        .unwrap();
        assert!(!silent.notifications.audio);
    }

    #[test]
    fn jump_back_defaults_to_enabled() {
        assert!(Config::default().jump_back.enabled);
    }

    #[test]
    fn glance_defaults_and_overrides() {
        let default = Config::default().glance;
        assert_eq!(default.active_minutes, 15);
        assert_eq!(default.recent_hours, 4);
        assert_eq!(default.icons, IconStyle::Unicode);

        let custom: Config = toml::from_str(
            r#"
            [glance]
            active_minutes = 5
            icons = "nerd"
            "#,
        )
        .unwrap();
        assert_eq!(custom.glance.active_minutes, 5);
        assert_eq!(custom.glance.recent_hours, 4);
        assert_eq!(custom.glance.icons, IconStyle::Nerd);
    }

    #[test]
    fn parses_jump_back_config() {
        let config: Config = toml::from_str(
            r#"
            [jump_back]
            enabled = false
            "#,
        )
        .unwrap();

        assert!(!config.jump_back.enabled);
    }

    #[test]
    fn parses_command_integration_config() {
        let config: Config = toml::from_str(
            r#"
            [[integrations]]
            id = "bookmarks"
            label = "Bookmarks"
            collect = "bookmarks list --json"
            open = "bookmarks open {{id}}"
            notify_success = false
            "#,
        )
        .unwrap();

        assert_eq!(config.integrations.len(), 1);
        assert_eq!(config.integrations[0].id, "bookmarks");
        assert_eq!(config.integrations[0].label, "Bookmarks");
        assert!(!config.integrations[0].notify_success);
        assert!(config.integrations[0].notify_error);
    }

    #[test]
    fn custom_filter_key_overrides_default_source_key() {
        let config: Config = toml::from_str(
            r#"
            [picker.filter_keys]
            zoxide = "ctrl-g"
            "#,
        )
        .unwrap();

        assert_eq!(
            config.picker.filter_source_for_key('g'),
            Some(Source::Zoxide)
        );
        assert_eq!(config.picker.filter_source_for_key('z'), None);
        assert_eq!(config.picker.filter_key(&Source::Zoxide), Some('g'));
    }

    #[test]
    fn retired_source_keys_parse_and_are_ignored() {
        let config: Config = toml::from_str(
            r#"
            [picker]
            agent_sort = "priority"
            source_order = ["agent", "project", "workspace", "zoxide"]

            [picker.filter_keys]
            agent = "ctrl-g"
            project = "ctrl-h"

            [jump_back]
            pin_previous = true

            [sources]
            agents = true
            herdr_plus_projects = true

            [[agent_aliases]]
            alias = "main ai dot"
            agent = "claude"
            "#,
        )
        .unwrap();

        assert!(config.jump_back.enabled);
        let sources = config.enabled_sources_in_order();
        assert_eq!(sources[0], Source::Workspace);
        assert_eq!(sources[1], Source::Zoxide);
        assert_eq!(config.picker.filter_source_for_key('g'), None);
        assert_eq!(config.picker.filter_source_for_key('h'), None);
    }
}
