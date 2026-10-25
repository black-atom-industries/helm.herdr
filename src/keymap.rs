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
    SelectLabel,
    OpenTab,
    OpenPrevious,
    MoveUp,
    MoveDown,
    NextGroup,
    PrevGroup,
    NextNeedsYou,
    ScrollDetailUp,
    ScrollDetailDown,
    StartSearch,
    DeleteChar,
    DeleteWord,
    Clear,
    CloseWorkspace,
    ArmClose,
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
    command: Command,
}

pub(crate) struct Keybind {
    pub(crate) label: String,
    pub(crate) group: &'static str,
    compact_label: Option<&'static str>,
    /// Shown instead of the joined key labels, for bindings that cover a family of keys.
    key_text: Option<String>,
    /// Footer hints on the right edge instead of the left block.
    pub(crate) footer_right: bool,
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
    /// The command of the binding's first key, which names the binding.
    pub(crate) fn command(&self) -> Command {
        self.keys[0].command.clone()
    }

    pub(crate) fn command_for(&self, app: &App, key: KeyEvent) -> Option<Command> {
        self.keys
            .iter()
            .find(|spec| spec.matches(app, key))
            .map(|spec| spec.command.clone())
    }

    pub(crate) fn key_label(&self, _app: &App) -> String {
        if let Some(text) = &self.key_text {
            return text.clone();
        }
        let mut labels = Vec::new();
        for label in self.keys.iter().map(|spec| spec.label.as_str()) {
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        labels.join("/")
    }

    pub(crate) fn is_active(&self, app: &App) -> bool {
        match self.command() {
            Command::StartSearch => app.input_mode == InputMode::Search,
            Command::ToggleMark => app
                .selected_entry()
                .is_some_and(|entry| app.is_pinned(entry)),
            Command::ToggleHelp => app.input_mode == InputMode::Help,
            Command::Filter(source) => {
                app.source_filter.as_ref().unwrap_or(&Source::Workspace) == &source
            }
            _ => false,
        }
    }

    pub(crate) fn compact_hint(&self, app: &App) -> Option<(String, &'static str)> {
        let label = self.compact_label?;
        Some((self.key_label(app), label))
    }

    fn keys_as(mut self, text: impl Into<String>) -> Self {
        self.key_text = Some(text.into());
        self
    }

    fn on_right(mut self) -> Self {
        self.footer_right = true;
        self
    }
}

fn key(
    command: Command,
    code: KeyCode,
    modifiers: KeyModifiers,
    label: impl Into<String>,
) -> KeySpec {
    KeySpec {
        code,
        modifiers,
        label: label.into(),
        scope: Scope::Always,
        command,
    }
}

fn directory_template_key(value: &str) -> Option<KeySpec> {
    match value.trim().to_ascii_lowercase().as_str() {
        "alt-enter" | "alt+enter" | "option-enter" | "option+enter" => Some(key(
            Command::OpenTemplate,
            KeyCode::Enter,
            KeyModifiers::ALT,
            "alt-enter",
        )),
        _ => {
            let key_char = parse_ctrl_key(value)?;
            Some(key(
                Command::OpenTemplate,
                KeyCode::Char(key_char),
                KeyModifiers::CONTROL,
                format!("^{}", key_char.to_ascii_uppercase()),
            ))
        }
    }
}

fn normal_key(command: Command, code: KeyCode, label: impl Into<String>) -> KeySpec {
    KeySpec {
        code,
        modifiers: KeyModifiers::NONE,
        label: label.into(),
        scope: Scope::Normal,
        command,
    }
}

/// The SHIFT form of a character key. Terminals send `!`, `J` and `K` with
/// the modifier set, so these need their own spec beside the NONE form.
fn normal_shifted(command: Command, code: KeyCode, label: impl Into<String>) -> KeySpec {
    KeySpec {
        modifiers: KeyModifiers::SHIFT,
        ..normal_key(command, code, label)
    }
}

fn binding(
    keys: Vec<KeySpec>,
    label: impl Into<String>,
    group: &'static str,
    compact_label: Option<&'static str>,
) -> Keybind {
    Keybind {
        label: label.into(),
        group,
        compact_label,
        key_text: None,
        footer_right: false,
        keys,
    }
}

pub(crate) fn keybindings(app: &App) -> Vec<Keybind> {
    let labels = app.config.glance.labels();
    let mut bindings = Vec::new();

    if let (Some(first), Some(last)) = (labels.first(), labels.last()) {
        let keys = labels
            .iter()
            .map(|ch| normal_key(Command::SelectLabel, KeyCode::Char(*ch), ch.to_string()))
            .collect();
        let range = if first == last {
            first.to_string()
        } else {
            format!("{first}–{last}")
        };
        bindings.push(binding(keys, "select labelled row", "Move", Some("row")).keys_as(range));
    }

    let digits = ('1'..='9')
        .map(|ch| normal_key(Command::OpenTab, KeyCode::Char(ch), ch.to_string()))
        .collect();
    bindings.push(binding(digits, "open tab of selected row", "Open", Some("tab")).keys_as("1–9"));

    bindings.push(binding(
        vec![key(
            Command::Open,
            KeyCode::Enter,
            KeyModifiers::NONE,
            "enter",
        )],
        "open at last active tab",
        "Open",
        Some("open"),
    ));

    bindings.push(binding(
        vec![normal_key(Command::OpenPrevious, KeyCode::Char('l'), "l")],
        "open previous workspace",
        "Open",
        Some("last"),
    ));

    bindings.push(
        binding(
            vec![
                normal_key(Command::ArmClose, KeyCode::Char('x'), "x"),
                key(
                    Command::CloseWorkspace,
                    KeyCode::Char('x'),
                    KeyModifiers::CONTROL,
                    "^X",
                ),
            ],
            "close workspace (x twice, ^X at once)",
            "Act",
            Some("close"),
        )
        .keys_as("x"),
    );

    bindings.push(
        binding(
            vec![
                normal_key(Command::NextGroup, KeyCode::Char(']'), "]"),
                normal_key(Command::PrevGroup, KeyCode::Char('['), "["),
            ],
            "next / previous group",
            "Move",
            Some("group"),
        )
        .keys_as("] ["),
    );

    bindings.push(binding(
        vec![
            normal_key(Command::NextNeedsYou, KeyCode::Char('!'), "!"),
            normal_shifted(Command::NextNeedsYou, KeyCode::Char('!'), "!"),
        ],
        "next that needs you",
        "Move",
        Some("needs you"),
    ));

    bindings.push(binding(
        vec![key(
            Command::StartSearch,
            KeyCode::Char('/'),
            KeyModifiers::NONE,
            "/",
        )],
        "search",
        "Find",
        Some("search"),
    ));

    bindings.push(binding(
        vec![key(
            Command::ToggleMark,
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
            "^B",
        )],
        "pin to the top",
        "Act",
        Some("pin"),
    ));

    bindings.push(
        binding(
            vec![
                key(Command::MoveDown, KeyCode::Down, KeyModifiers::NONE, "↓"),
                normal_key(Command::MoveDown, KeyCode::Char('j'), "j"),
                key(Command::MoveUp, KeyCode::Up, KeyModifiers::NONE, "↑"),
                normal_key(Command::MoveUp, KeyCode::Char('k'), "k"),
            ],
            "next / previous row",
            "Move",
            None,
        )
        .keys_as("j k"),
    );

    bindings.push(
        binding(
            vec![
                normal_key(Command::ScrollDetailDown, KeyCode::Char('J'), "J"),
                normal_shifted(Command::ScrollDetailDown, KeyCode::Char('J'), "J"),
                normal_key(Command::ScrollDetailUp, KeyCode::Char('K'), "K"),
                normal_shifted(Command::ScrollDetailUp, KeyCode::Char('K'), "K"),
            ],
            "scroll the detail pane",
            "Move",
            None,
        )
        .keys_as("J K"),
    );

    if app.directory_template_for_selected().is_some() {
        if let Some(template_key) =
            directory_template_key(&app.config.picker.directory_template_key)
        {
            bindings.push(binding(
                vec![template_key],
                "open selected with directory template",
                "Open",
                None,
            ));
        }
    }

    bindings.push(binding(
        vec![key(
            Command::Clear,
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
            "^U",
        )],
        "clear query and filter",
        "Find",
        None,
    ));
    bindings.push(binding(
        vec![key(
            Command::DeleteChar,
            KeyCode::Backspace,
            KeyModifiers::NONE,
            "backspace",
        )],
        "delete query character",
        "Find",
        None,
    ));
    bindings.push(binding(
        vec![
            // Terminals send Ctrl-Backspace as 0x08, which crossterm decodes
            // as Ctrl-H. Under the kitty keyboard protocol it arrives as a
            // real Ctrl-Backspace instead, so accept both spellings.
            key(
                Command::DeleteWord,
                KeyCode::Backspace,
                KeyModifiers::CONTROL,
                "^backspace",
            ),
            key(
                Command::DeleteWord,
                KeyCode::Char('h'),
                KeyModifiers::CONTROL,
                "^backspace",
            ),
        ],
        "delete query word",
        "Find",
        None,
    ));

    for source in app.config.enabled_sources_in_order() {
        let Some(filter_key) = app.config.picker.filter_key(&source) else {
            continue;
        };
        bindings.push(binding(
            vec![key(
                Command::Filter(source.clone()),
                KeyCode::Char(filter_key),
                KeyModifiers::CONTROL,
                format!("^{}", filter_key.to_ascii_uppercase()),
            )],
            source_help_label(&source),
            "Sources",
            None,
        ));
    }

    if app.update_available.is_some() {
        bindings.push(binding(
            vec![key(
                Command::Update,
                KeyCode::F(5),
                KeyModifiers::NONE,
                "F5",
            )],
            "install available update",
            "Act",
            None,
        ));
    }

    bindings.push(
        binding(
            vec![
                key(
                    Command::ToggleHelp,
                    KeyCode::Char('?'),
                    KeyModifiers::NONE,
                    "?",
                ),
                key(
                    Command::ToggleHelp,
                    KeyCode::Char('?'),
                    KeyModifiers::SHIFT,
                    "?",
                ),
            ],
            "this sheet",
            "Act",
            Some("keys"),
        )
        .on_right(),
    );
    bindings.push(
        binding(
            vec![
                key(Command::Back, KeyCode::Esc, KeyModifiers::NONE, "Esc"),
                normal_key(Command::Back, KeyCode::Char('q'), "q"),
                key(
                    Command::Back,
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                    "^C",
                ),
            ],
            "back, then close",
            "Open",
            Some("quit"),
        )
        .keys_as("q")
        .on_right(),
    );
    bindings
}

fn source_help_label(source: &Source) -> &'static str {
    match source {
        Source::Workspace => "workspaces",
        Source::Zoxide => "zoxide",
        Source::Root => "roots",
        Source::QuickAction => "quick actions",
        Source::Integration => "plugins",
    }
}
