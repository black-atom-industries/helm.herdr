use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::{
    app::{App, InputMode},
    config::parse_ctrl_key,
    model::Source,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Back,
    Open,
    OpenTemplate,
    MoveUp,
    MoveDown,
    Collapse,
    Expand,
    StartSearch,
    DeleteChar,
    DeleteWord,
    Clear,
    CloseWorkspace,
    ToggleMark,
    ToggleHelp,
    Update,
    Filter(Source),
}

#[derive(Clone, Copy)]
enum Scope {
    Always,
    Normal,
}

#[derive(Clone)]
struct KeySpec {
    code: KeyCode,
    modifiers: KeyModifiers,
    label: String,
    scope: Scope,
}

pub(crate) struct Keybind {
    pub(crate) command: Command,
    pub(crate) label: String,
    pub(crate) group: &'static str,
    compact_label: Option<&'static str>,
    keys: Vec<KeySpec>,
}

impl KeySpec {
    fn matches(&self, app: &App, key: KeyEvent) -> bool {
        let modifiers_match = if self.modifiers.is_empty() {
            key.modifiers.is_empty()
        } else {
            key.modifiers.contains(self.modifiers)
        };
        self.scope.enabled(app) && self.code == key.code && modifiers_match
    }

    fn visible(&self, _app: &App) -> bool {
        match self.scope {
            Scope::Always | Scope::Normal => true,
        }
    }
}

impl Scope {
    fn enabled(self, app: &App) -> bool {
        match self {
            Self::Always => true,
            Self::Normal => app.input_mode == InputMode::Normal,
        }
    }
}

impl Keybind {
    pub(crate) fn matches(&self, app: &App, key: KeyEvent) -> bool {
        self.keys.iter().any(|spec| spec.matches(app, key))
    }

    pub(crate) fn key_label(&self, app: &App) -> String {
        let mut labels = Vec::new();
        for label in self
            .keys
            .iter()
            .filter(|spec| spec.visible(app))
            .map(|spec| spec.label.as_str())
        {
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        labels.join("/")
    }

    pub(crate) fn is_active(&self, app: &App) -> bool {
        match &self.command {
            Command::StartSearch => app.input_mode == InputMode::Search,
            Command::ToggleMark => {
                let entry = if app.topology_view() {
                    app.topology_selected_entry()
                } else {
                    app.selected_entry()
                };
                entry.is_some_and(|entry| app.is_pinned(entry))
            }
            Command::ToggleHelp => app.input_mode == InputMode::Help,
            Command::Filter(source) => app.source_filter.as_ref() == Some(source),
            _ => false,
        }
    }

    pub(crate) fn compact_hint(&self, app: &App) -> Option<(String, &'static str)> {
        let label = self.compact_label?;
        let key = match &self.command {
            Command::MoveDown => "↓/j".into(),
            _ => self.key_label(app),
        };
        Some((key, label))
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers, label: impl Into<String>) -> KeySpec {
    KeySpec {
        code,
        modifiers,
        label: label.into(),
        scope: Scope::Always,
    }
}

fn directory_template_key(value: &str) -> Option<KeySpec> {
    match value.trim().to_ascii_lowercase().as_str() {
        "alt-enter" | "alt+enter" | "option-enter" | "option+enter" => {
            Some(key(KeyCode::Enter, KeyModifiers::ALT, "⌥↵"))
        }
        _ => {
            let key_char = parse_ctrl_key(value)?;
            Some(key(
                KeyCode::Char(key_char),
                KeyModifiers::CONTROL,
                format!("⌃{}", key_char.to_ascii_uppercase()),
            ))
        }
    }
}

fn normal_key(code: KeyCode, label: impl Into<String>) -> KeySpec {
    KeySpec {
        code,
        modifiers: KeyModifiers::NONE,
        label: label.into(),
        scope: Scope::Normal,
    }
}

fn binding(
    command: Command,
    keys: Vec<KeySpec>,
    label: impl Into<String>,
    group: &'static str,
    compact_label: Option<&'static str>,
) -> Keybind {
    Keybind {
        command,
        keys,
        label: label.into(),
        group,
        compact_label,
    }
}

pub(crate) fn keybindings(app: &App) -> Vec<Keybind> {
    let mut bindings = vec![
        binding(
            Command::Back,
            vec![
                key(KeyCode::Esc, KeyModifiers::NONE, "Esc"),
                key(KeyCode::Char('c'), KeyModifiers::CONTROL, "⌃C"),
            ],
            "back / close",
            "Actions",
            None,
        ),
        binding(
            Command::MoveUp,
            vec![
                key(KeyCode::Up, KeyModifiers::NONE, "↑"),
                normal_key(KeyCode::Char('k'), "k"),
            ],
            "move up",
            "Navigation",
            None,
        ),
        binding(
            Command::MoveDown,
            vec![
                key(KeyCode::Down, KeyModifiers::NONE, "↓"),
                normal_key(KeyCode::Char('j'), "j"),
            ],
            "move down",
            "Navigation",
            Some("up/down"),
        ),
        binding(
            Command::Collapse,
            vec![
                key(KeyCode::Left, KeyModifiers::NONE, "←"),
                normal_key(KeyCode::Char('h'), "h"),
            ],
            "collapse / parent",
            "Navigation",
            Some("collapse"),
        ),
        binding(
            Command::Expand,
            vec![
                key(KeyCode::Right, KeyModifiers::NONE, "→"),
                normal_key(KeyCode::Char('l'), "l"),
            ],
            "expand",
            "Navigation",
            Some("expand"),
        ),
    ];

    for source in app.config.enabled_sources_in_order() {
        let Some(filter_key) = app.config.picker.filter_key(&source) else {
            continue;
        };
        bindings.push(binding(
            Command::Filter(source.clone()),
            vec![key(
                KeyCode::Char(filter_key),
                KeyModifiers::CONTROL,
                format!("⌃{}", filter_key.to_ascii_uppercase()),
            )],
            source_help_label(&source),
            "Filters",
            Some(source_compact_label(&source)),
        ));
    }

    bindings.extend([
        binding(
            Command::Open,
            vec![key(KeyCode::Enter, KeyModifiers::NONE, "↵")],
            "open selected",
            "Actions",
            Some("open"),
        ),
        binding(
            Command::StartSearch,
            vec![key(KeyCode::Char('/'), KeyModifiers::NONE, "/")],
            "search",
            "Actions",
            Some("search"),
        ),
        binding(
            Command::DeleteChar,
            vec![key(KeyCode::Backspace, KeyModifiers::NONE, "⌫")],
            "delete query character",
            "Actions",
            None,
        ),
        binding(
            Command::DeleteWord,
            vec![
                // Terminals send Ctrl-Backspace as 0x08, which crossterm decodes
                // as Ctrl-H. Under the kitty keyboard protocol it arrives as a
                // real Ctrl-Backspace instead, so accept both spellings.
                key(KeyCode::Backspace, KeyModifiers::CONTROL, "⌃⌫"),
                key(KeyCode::Char('h'), KeyModifiers::CONTROL, "⌃⌫"),
            ],
            "delete query word",
            "Actions",
            None,
        ),
        binding(
            Command::Clear,
            vec![key(KeyCode::Char('u'), KeyModifiers::CONTROL, "⌃U")],
            "clear query and filter",
            "Actions",
            None,
        ),
        binding(
            Command::CloseWorkspace,
            vec![key(KeyCode::Char('x'), KeyModifiers::CONTROL, "⌃X")],
            "close workspace",
            "Actions",
            None,
        ),
        binding(
            Command::ToggleMark,
            vec![key(KeyCode::Char('b'), KeyModifiers::CONTROL, "⌃B")],
            "mark / unmark selected",
            "Actions",
            Some("mark"),
        ),
        binding(
            Command::ToggleHelp,
            vec![
                key(KeyCode::Char('?'), KeyModifiers::NONE, "?"),
                key(KeyCode::Char('?'), KeyModifiers::SHIFT, "?"),
            ],
            "keybindings",
            "View",
            Some("keys"),
        ),
    ]);
    if app.directory_template_for_selected().is_some() {
        if let Some(template_key) =
            directory_template_key(&app.config.picker.directory_template_key)
        {
            bindings.push(binding(
                Command::OpenTemplate,
                vec![template_key],
                "open selected with directory template",
                "Actions",
                Some("template"),
            ));
        }
    }
    if app.update_available.is_some() {
        bindings.push(binding(
            Command::Update,
            vec![key(KeyCode::F(5), KeyModifiers::NONE, "F5")],
            "install available update",
            "Actions",
            None,
        ));
    }
    bindings
}

fn source_help_label(source: &Source) -> &'static str {
    match source {
        Source::Workspace => "Workspaces / Tabs / Panes",
        Source::Project => "projects",
        Source::Zoxide => "zoxide",
        Source::Root => "roots",
        Source::Agent => "agents",
        Source::Server => "servers",
        Source::Session => "sessions",
        Source::QuickAction => "quick actions",
        Source::Integration => "plugins",
    }
}

fn source_compact_label(source: &Source) -> &'static str {
    match source {
        Source::Workspace => "Workspaces / Tabs / Panes",
        Source::Project => "project",
        Source::Zoxide => "zoxide",
        Source::Root => "root",
        Source::Agent => "agent",
        Source::Server => "server",
        Source::Session => "session",
        Source::QuickAction => "quick",
        Source::Integration => "plugin",
    }
}
