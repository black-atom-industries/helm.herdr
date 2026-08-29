use std::{
    io,
    sync::mpsc::{Receiver, TryRecvError},
    time::Duration,
};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};

use crate::{
    app::{App, InputMode},
    keymap::{keybindings, Command},
    model::{Entry, EntryAction, Source},
    paths::home,
    sources::status_icon_at,
    theme::Theme,
    topology::{repository_color, topology_lines_selected, TopologyDepth, WorkspaceNode},
};

fn open_action_error(error: String, persist: bool) -> io::Result<()> {
    eprintln!("{error}");
    if persist {
        wait_for_key();
        Ok(())
    } else {
        Err(io::Error::other(error))
    }
}

pub(crate) fn tui_loop(
    app: &mut App,
    persist: bool,
    mut update_check: Option<Receiver<Option<String>>>,
) -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let mut list_hits = ListHits::default();
    let result = loop {
        if let Some(result) = update_check.as_ref().map(Receiver::try_recv) {
            match result {
                Ok(version) => {
                    app.update_available = version;
                    update_check = None;
                }
                Err(TryRecvError::Disconnected) => update_check = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        terminal.draw(|f| list_hits = draw(f, app))?;
        let animate = has_working_entry(app);
        if (animate || update_check.is_some()) && !event::poll(Duration::from_millis(125))? {
            if animate {
                app.spinner_tick = app.spinner_tick.wrapping_add(1);
            }
            continue;
        }
        let action = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => handle_key(app, key),
            Event::Mouse(mouse) => handle_mouse(app, mouse, &list_hits),
            _ => Action::Continue,
        };
        match action {
            Action::Continue => {}
            Action::Quit => break Ok(()),
            action @ (Action::Open | Action::OpenTemplate) => {
                // leave the TUI while the action runs: herdr CLI output goes to
                // the normal screen instead of corrupting the alternate screen
                cleanup_terminal(&mut terminal)?;
                let outcome = app.open_selected(matches!(action, Action::OpenTemplate));
                if let Err(e) = outcome {
                    open_action_error(e, persist)?;
                }
                if !persist {
                    return Ok(());
                }
                app.refresh();
                enable_raw_mode()?;
                execute!(
                    terminal.backend_mut(),
                    EnterAlternateScreen,
                    EnableMouseCapture
                )?;
                terminal.clear()?;
            }
            Action::Update => {
                cleanup_terminal(&mut terminal)?;
                if let Some(version) = app.update_available.clone() {
                    if confirm_update(&version)? {
                        match crate::update::install(&version) {
                            Ok(()) => eprintln!("Updated Helm to v{version}."),
                            Err(error) => eprintln!("Update failed: {error}"),
                        }
                        wait_for_key();
                        return Ok(());
                    }
                }
                enable_raw_mode()?;
                execute!(
                    terminal.backend_mut(),
                    EnterAlternateScreen,
                    EnableMouseCapture
                )?;
                terminal.clear()?;
            }
            Action::CloseWorkspace => {
                if let Err(e) = app.close_selected_workspace() {
                    crate::herdr::notify_error(
                        &format!("Close failed: {e}"),
                        &app.config.notifications,
                    );
                }
            }
        }
    };
    cleanup_terminal(&mut terminal)?;
    result
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn confirm_update(version: &str) -> io::Result<bool> {
    eprintln!("Update Helm to v{version}? [y/N]");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn wait_for_key() {
    eprintln!("press enter to close...");
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
}

enum Action {
    Continue,
    Quit,
    Open,
    OpenTemplate,
    Update,
    CloseWorkspace,
}

#[derive(Default)]
struct ListHits {
    area: Rect,
    rows: Vec<(std::ops::Range<u16>, usize)>,
    topology: Vec<TopologyHit>,
}

struct TopologyHit {
    y: std::ops::Range<u16>,
    x: std::ops::Range<u16>,
    workspace: usize,
    line: usize,
    tab: usize,
    child: Option<usize>,
}

fn handle_mouse(app: &mut App, mouse: MouseEvent, hits: &ListHits) -> Action {
    if app.input_mode == InputMode::Help || !hits.area.contains((mouse.column, mouse.row).into()) {
        return Action::Continue;
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if app.topology_view() {
                app.topology_move_vertical(-1);
            } else {
                app.prev();
            }
        }
        MouseEventKind::ScrollDown => {
            if app.topology_view() {
                app.topology_move_vertical(1);
            } else {
                app.next();
            }
        }
        MouseEventKind::Down(MouseButton::Left) if app.topology_view() => {
            let Some(hit) = hits
                .topology
                .iter()
                .find(|hit| hit.y.contains(&mouse.row) && hit.x.contains(&mouse.column))
            else {
                return Action::Continue;
            };
            let before = (
                app.topology_cursor.workspace,
                app.topology_cursor.depth,
                app.topology_cursor.selection[app.topology_cursor.workspace],
            );
            app.topology_cursor.workspace = hit.workspace;
            match hit.line {
                0 => app.topology_cursor.leave_to_workspace(),
                1 => {
                    app.topology_cursor.depth = TopologyDepth::Tab;
                    if let Some(tab) = hit.child {
                        app.topology_cursor.selection[hit.workspace].tab = tab;
                    }
                }
                2 | 3 => {
                    app.topology_cursor.depth = TopologyDepth::Pane;
                    app.topology_cursor.selection[hit.workspace].tab = hit.tab;
                    if let Some(pane) = hit.child {
                        app.topology_cursor.selection[hit.workspace].pane = pane;
                    }
                }
                _ => {}
            }
            app.topology_cursor.clamp(&app.topology);
            app.remember_topology_selection();
            let after = (
                app.topology_cursor.workspace,
                app.topology_cursor.depth,
                app.topology_cursor.selection[app.topology_cursor.workspace],
            );
            return if before == after {
                Action::Open
            } else {
                Action::Continue
            };
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let Some((_, row)) = hits
                .rows
                .iter()
                .find(|(range, _)| range.contains(&mouse.row))
            else {
                return Action::Continue;
            };
            if app.selected == *row {
                return Action::Open;
            }
            app.selected = *row;
        }
        _ => {}
    }
    Action::Continue
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    if app.input_mode != InputMode::Help
        && !app.topology_view()
        && matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
    {
        return Action::Continue;
    }

    if app.input_mode == InputMode::Normal && app.topology_view() {
        match (key.code, key.modifiers) {
            (KeyCode::Char('['), KeyModifiers::NONE) => {
                app.topology_move_workspace(-1);
                return Action::Continue;
            }
            (KeyCode::Char(']'), KeyModifiers::NONE) => {
                app.topology_move_workspace(1);
                return Action::Continue;
            }
            (KeyCode::Char('K'), KeyModifiers::SHIFT) => {
                app.topology_move_workspace(-1);
                return Action::Continue;
            }
            (KeyCode::Char('J'), KeyModifiers::SHIFT) => {
                app.topology_move_workspace(1);
                return Action::Continue;
            }
            (KeyCode::Char('h'), KeyModifiers::NONE) => {
                app.topology_move_horizontal(-1);
                return Action::Continue;
            }
            (KeyCode::Char('l'), KeyModifiers::NONE) => {
                app.topology_move_horizontal(1);
                return Action::Continue;
            }
            (KeyCode::Char('j'), KeyModifiers::NONE) => {
                app.topology_move_vertical(1);
                return Action::Continue;
            }
            (KeyCode::Char('k'), KeyModifiers::NONE) => {
                app.topology_move_vertical(-1);
                return Action::Continue;
            }
            (KeyCode::Tab, KeyModifiers::NONE) => {
                app.topology_move_horizontal(1);
                return Action::Continue;
            }
            (KeyCode::BackTab, KeyModifiers::SHIFT) | (KeyCode::Tab, KeyModifiers::SHIFT) => {
                app.topology_move_horizontal(-1);
                return Action::Continue;
            }
            (KeyCode::Enter, KeyModifiers::NONE) => return Action::Open,
            _ => {}
        }
    }

    let command = keybindings(app)
        .into_iter()
        .find(|binding| binding.matches(app, key))
        .map(|binding| binding.command);

    if app.input_mode == InputMode::Help {
        if matches!(command, Some(Command::Back | Command::ToggleHelp)) {
            app.input_mode = InputMode::Normal;
        }
        return Action::Continue;
    }

    if let Some(command) = command {
        return execute_command(app, command, key);
    }

    // Only plain and shifted characters are text. Without this guard every
    // unbound chord inserts its letter -- Ctrl-Backspace arrives as Ctrl-H on
    // most terminals and used to type an "h".
    if app.input_mode == InputMode::Search {
        if let KeyCode::Char(c) = key.code {
            if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
                app.query.push(c);
                app.apply_filter();
            }
        }
    }
    Action::Continue
}

fn execute_command(app: &mut App, command: Command, key: KeyEvent) -> Action {
    match command {
        Command::Back => {
            if key.code == KeyCode::Esc && app.input_mode == InputMode::Search {
                app.input_mode = InputMode::Normal;
                Action::Continue
            } else {
                Action::Quit
            }
        }
        Command::Open => {
            if app.topology_view() {
                Action::Open
            } else {
                app.selected_entry()
                    .map(|_| Action::Open)
                    .unwrap_or(Action::Continue)
            }
        }
        Command::OpenTemplate => {
            if app.topology_view() {
                Action::OpenTemplate
            } else {
                app.selected_entry()
                    .map(|_| Action::OpenTemplate)
                    .unwrap_or(Action::Continue)
            }
        }
        Command::Update => Action::Update,
        Command::MoveUp => {
            app.topology_move_vertical(-1);
            Action::Continue
        }
        Command::MoveDown => {
            app.topology_move_vertical(1);
            Action::Continue
        }
        Command::Collapse => {
            if app.topology_view() {
                match app.topology_cursor.depth {
                    TopologyDepth::Workspace => {}
                    TopologyDepth::Tab => app.topology_cursor.leave_to_workspace(),
                    TopologyDepth::Pane => app.topology_cursor.leave_to_tab(),
                }
            }
            Action::Continue
        }
        Command::Expand => {
            if app.topology_view() {
                app.topology_move_horizontal(1);
            }
            Action::Continue
        }
        Command::StartSearch => {
            app.input_mode = InputMode::Search;
            Action::Continue
        }
        Command::DeleteChar => {
            app.query.pop();
            app.apply_filter();
            Action::Continue
        }
        Command::DeleteWord => {
            app.delete_query_word();
            app.apply_filter();
            Action::Continue
        }
        Command::Clear => {
            app.query.clear();
            app.set_filter(None);
            app.input_mode = InputMode::Normal;
            app.apply_filter();
            Action::Continue
        }
        Command::CloseWorkspace => Action::CloseWorkspace,
        Command::ToggleMark => {
            if let Err(error) = app.toggle_selected_pin() {
                crate::herdr::notify_error(
                    &format!("Mark failed: {error}"),
                    &app.config.notifications,
                );
            }
            Action::Continue
        }
        Command::ToggleHelp => {
            app.input_mode = InputMode::Help;
            Action::Continue
        }
        Command::Filter(source) => {
            if !key
                .modifiers
                .contains(crossterm::event::KeyModifiers::CONTROL)
            {
                app.query.clear();
                app.input_mode = InputMode::Normal;
            }
            app.set_filter(Some(source));
            app.apply_filter();
            Action::Continue
        }
    }
}

fn draw(f: &mut Frame, app: &App) -> ListHits {
    let area = f.area();
    f.render_widget(Clear, area);
    let inner = area;

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if app.input_mode == InputMode::Search {
                3
            } else {
                1
            }),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

    if app.input_mode == InputMode::Search {
        let filter = app
            .source_filter
            .as_ref()
            .map(|s| s.label())
            .unwrap_or("all");
        let mut search_spans = vec![
            Span::styled("query ", Style::default().fg(app.theme.overlay0)),
            Span::styled(
                &app.query,
                Style::default()
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                format!("filter:{filter}"),
                Style::default().fg(app.theme.accent),
            ),
        ];
        if let Some(version) = &app.update_available {
            search_spans.push(Span::styled(
                format!("   ↑ v{version} available · F5 update"),
                Style::default()
                    .fg(app.theme.yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let search = Paragraph::new(Line::from(search_spans)).block(
            Block::default()
                .title(" Helm ")
                .style(Style::default().bg(app.theme.panel_bg))
                .borders(Borders::BOTTOM),
        );
        f.render_widget(search, rows[0]);
    } else {
        let title = app.update_available.as_ref().map_or_else(
            || " Helm ".to_string(),
            |version| format!(" Helm   ↑ v{version} available · F5 update "),
        );
        f.render_widget(
            Block::default()
                .title(title)
                .style(Style::default().bg(app.theme.panel_bg)),
            rows[0],
        );
    }

    let list_hits = draw_list(f, app, rows[1]);

    draw_key_hints(f, app, rows[2]);
    if app.input_mode == InputMode::Help {
        draw_keybindings_help(f, app, area);
    }
    list_hits
}

fn draw_key_hints(f: &mut Frame, app: &App, area: Rect) {
    let mut command_spans = Vec::new();
    let mut filter_spans = Vec::new();
    for binding in keybindings(app) {
        let Some((key, label)) = binding.compact_hint(app) else {
            continue;
        };
        if key.is_empty() {
            continue;
        }
        let spans = if binding.group == "Filters" {
            &mut filter_spans
        } else {
            &mut command_spans
        };
        let active = binding.is_active(app);
        let key_style = if active {
            Style::default()
                .fg(app.theme.panel_bg)
                .bg(app.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(format!(" {key} "), key_style));
        spans.push(Span::styled(
            format!("{label}  "),
            Style::default().fg(if active {
                app.theme.text
            } else {
                app.theme.overlay0
            }),
        ));
    }
    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(command_spans),
            Line::from(filter_spans),
        ]))
        .style(Style::default().bg(app.theme.panel_bg)),
        area,
    );
}

fn draw_keybindings_help(f: &mut Frame, app: &App, area: Rect) {
    let bindings = keybindings(app);
    let mut lines = Vec::new();
    for group in ["Navigation", "Actions", "View", "Filters"] {
        let start = lines.len();
        lines.push(Line::styled(
            format!(" {group}"),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
        for binding in bindings.iter().filter(|binding| binding.group == group) {
            let key = binding.key_label(app);
            if key.is_empty() {
                continue;
            }
            let active = binding.is_active(app);
            let key_style = if active {
                Style::default()
                    .fg(app.theme.panel_bg)
                    .bg(app.theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.accent)
            };
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{key:<12}"), key_style),
                Span::styled(&binding.label, Style::default().fg(app.theme.text)),
            ]));
        }
        if lines.len() == start + 1 {
            lines.pop();
        } else {
            lines.push(Line::default());
        }
    }
    lines.pop();

    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2).max(1));
    let popup = area.centered(Constraint::Percentage(72), Constraint::Length(height));
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(app.theme.panel_bg))
            .block(
                Block::default()
                    .title(" Keybindings ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.theme.accent)),
            ),
        popup,
    );
}

fn has_working_entry(app: &App) -> bool {
    app.entries.iter().filter_map(entry_status).any(|status| {
        let status = status.to_lowercase();
        status.contains("work") || status.contains("run")
    })
}

fn agent_status_color(theme: &Theme, status: &str) -> Color {
    let status = status.to_lowercase();
    if status.contains("block")
        || status.contains("error")
        || status.contains("fail")
        || status.contains("attention")
        || status.contains("request")
        || status.contains("wait")
    {
        theme.red
    } else if status.contains("work") || status.contains("run") {
        theme.yellow
    } else if status.contains("done") || status.contains("complete") {
        theme.teal
    } else if status.contains("idle") {
        theme.green
    } else {
        theme.overlay0
    }
}

fn is_topology_entry(entry: &Entry) -> bool {
    matches!(
        &entry.action,
        EntryAction::FocusWorkspace { .. }
            | EntryAction::FocusTab { .. }
            | EntryAction::FocusPane { .. }
    )
}

fn display_title(entry: &Entry) -> &str {
    if entry.source == Source::Workspace {
        entry
            .title
            .strip_prefix("dir: ")
            .or_else(|| entry.title.strip_prefix("project: "))
            .unwrap_or(&entry.title)
    } else {
        &entry.title
    }
}

fn entry_status(entry: &Entry) -> Option<&str> {
    if let Some(status) = entry
        .search_terms
        .iter()
        .find_map(|term| term.strip_prefix("agent-status:"))
    {
        return Some(status);
    }
    if entry.source == Source::Agent {
        return Some(
            entry
                .subtitle
                .split_once(" · ")
                .map(|(status, _)| status)
                .filter(|status| !status.is_empty())
                .unwrap_or("unknown"),
        );
    }
    entry.subtitle.strip_prefix("agent:").map(|rest| {
        rest.split_once(" · ")
            .map(|(status, _)| status)
            .unwrap_or(rest)
    })
}

fn entry_metadata(entry: &Entry) -> String {
    match entry.source {
        Source::Workspace => {
            let metadata = entry
                .subtitle
                .strip_prefix("agent:")
                .and_then(|rest| rest.split_once(" · ").map(|(_, metadata)| metadata))
                .unwrap_or(&entry.subtitle);
            let mut parts = metadata.split_whitespace();
            match (parts.next(), parts.next(), parts.next()) {
                (Some(_), Some(tabs), Some(panes)) => {
                    match (tabs.strip_prefix("tabs:"), panes.strip_prefix("panes:")) {
                        (Some(tabs), Some(panes)) => format!("{tabs} tabs · {panes} panes"),
                        _ => metadata.to_string(),
                    }
                }
                _ => metadata.to_string(),
            }
        }
        _ => entry.subtitle.clone(),
    }
}

fn display_path(entry: &Entry) -> String {
    entry
        .path
        .strip_prefix(home())
        .ok()
        .map(|path| {
            if path.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", path.display())
            }
        })
        .unwrap_or_else(|| entry.path.display().to_string())
}

fn metadata_width(width: u16) -> usize {
    if width >= 90 {
        28
    } else if width >= 68 {
        20
    } else if width >= 52 {
        14
    } else {
        0
    }
}

fn truncate_breadcrumb(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.into();
    }
    let parts = value.split(" › ").collect::<Vec<_>>();
    if parts.len() >= 3 {
        let endpoint = format!("{} › … › {}", parts[0], parts[parts.len() - 1]);
        if endpoint.chars().count() <= max_chars {
            return endpoint;
        }
    }
    truncate_end(value, max_chars)
}

fn truncate_end(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.into();
    }
    if max_chars == 0 {
        return String::new();
    }
    value
        .chars()
        .take(max_chars.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn truncate_terminal(value: &str, max_width: usize) -> String {
    let value_width = Span::raw(value).width();
    if value_width <= max_width {
        return value.into();
    }
    if max_width == 0 {
        return String::new();
    }
    let ellipsis = "…";
    let budget = max_width.saturating_sub(Span::raw(ellipsis).width());
    let mut result = String::new();
    let mut width: usize = 0;
    for character in value.chars() {
        let character_width = Span::raw(character.to_string()).width();
        if width.saturating_add(character_width) > budget {
            break;
        }
        result.push(character);
        width += character_width;
    }
    result.push_str(ellipsis);
    result
}

fn source_column_width(_app: &App) -> usize {
    9
}

fn source_spans(app: &App, entry: &Entry, width: usize) -> Vec<Span<'static>> {
    let source = truncate_terminal(entry.source_name(), width);
    let source_width = Span::raw(&source).width();
    vec![
        Span::styled(source, Style::default().fg(app.theme.overlay0)),
        Span::raw(" ".repeat(width.saturating_sub(source_width))),
    ]
}

fn entry_repository_key(entry: &Entry) -> Option<&str> {
    entry
        .search_terms
        .iter()
        .find_map(|term| term.strip_prefix("repo-key:"))
}

fn repository_keys(app: &App, entry: &Entry) -> Vec<String> {
    let mut keys = app
        .topology
        .workspaces
        .iter()
        .filter_map(|workspace| workspace.git.as_ref().map(|git| git.repo_key.clone()))
        .collect::<Vec<_>>();
    if let Some(key) = entry_repository_key(entry) {
        keys.push(key.into());
    }
    keys.sort();
    keys.dedup();
    keys
}

fn breadcrumb_spans(app: &App, entry: &Entry, value: &str) -> Vec<Span<'static>> {
    let Some(repository_key) = entry_repository_key(entry) else {
        return vec![Span::styled(
            value.to_string(),
            Style::default().fg(app.theme.overlay0),
        )];
    };
    let Some((parent, rest)) = value.split_once(" › ") else {
        return vec![Span::styled(
            value.to_string(),
            Style::default().fg(app.theme.overlay0),
        )];
    };
    vec![
        Span::styled(
            parent.to_string(),
            Style::default()
                .fg(repository_color(
                    repository_key,
                    &repository_keys(app, entry),
                    &app.theme,
                ))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" › {rest}"),
            Style::default().fg(app.theme.overlay0),
        ),
    ]
}

#[derive(Clone, Copy)]
struct DetailedLayout {
    status_width: usize,
    title_width: usize,
    right_width: usize,
}

fn detailed_layout(app: &App, row_width: usize, source_width: usize) -> DetailedLayout {
    let state_width = app
        .entries
        .iter()
        .filter_map(|entry| {
            entry_status(entry)
                .filter(|status| *status != "unknown")
                .map(|status| Span::raw(status).width())
        })
        .max()
        .unwrap_or(0);
    let status_width = state_width.saturating_add(2);
    let source_budget = source_width.saturating_add(1);
    let right_width = metadata_width(row_width.saturating_add(3) as u16).min(
        row_width.saturating_sub(source_budget.saturating_add(status_width).saturating_add(4)),
    );
    let right_separator = usize::from(right_width > 0);
    let content_width = row_width
        .saturating_sub(source_budget)
        .saturating_sub(status_width)
        .saturating_sub(right_width)
        .saturating_sub(right_separator);
    let title_width = content_width.min(48) / 2;
    DetailedLayout {
        status_width,
        title_width,
        right_width,
    }
}

fn topology_child_ranges(
    workspace: &WorkspaceNode,
    line: usize,
    selected_tab: usize,
    selected_child: Option<usize>,
    width: usize,
    start: u16,
) -> Vec<(std::ops::Range<u16>, usize)> {
    let values = if line == 1 {
        workspace
            .tabs
            .iter()
            .map(|tab| {
                if tab.label.is_empty() {
                    "unnamed".into()
                } else {
                    tab.label.clone()
                }
            })
            .collect::<Vec<_>>()
    } else {
        let Some(tab) = workspace.tabs.get(selected_tab) else {
            return Vec::new();
        };
        tab.panes
            .iter()
            .map(|pane| {
                if let Some(agent) = &pane.agent {
                    format!(
                        "{} {}",
                        agent.state.glyph(),
                        agent.alias.as_deref().unwrap_or(&agent.name)
                    )
                } else if pane.label.is_empty() {
                    "unnamed".into()
                } else {
                    pane.label.clone()
                }
            })
            .collect::<Vec<_>>()
    };
    let decorated = values
        .iter()
        .enumerate()
        .map(|(index, value)| crate::topology::selection_slot(value, selected_child == Some(index)))
        .collect::<Vec<_>>();
    let full = decorated.join(" ");
    let selected_value = selected_child.and_then(|index| decorated.get(index).map(String::as_str));
    let visible =
        crate::topology::clip_selected_for_hits(&full, width.saturating_sub(9), selected_value);
    let mut search_from = 0usize;
    decorated
        .into_iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let search_value = if selected_child == Some(index)
                && Span::raw(&value).width() > width.saturating_sub(9)
            {
                visible.as_str()
            } else {
                value.as_str()
            };
            let offset = visible[search_from..]
                .find(search_value)
                .map(|offset| search_from + offset)?;
            search_from = offset + search_value.len();
            let prefix = Span::raw(&visible[..offset]).width() as u16;
            let value_width = Span::raw(search_value).width() as u16;
            Some((start + prefix..start + prefix + value_width, index))
        })
        .collect()
}

fn draw_topology_list(f: &mut Frame, app: &App, area: Rect) -> ListHits {
    let block = Block::default()
        .title(" WORKSPACES ")
        .borders(Borders::RIGHT);
    let list_area = block.inner(area);
    let width = list_area.width as usize;
    let mut lines = Vec::new();
    let selected_workspace = app.topology_cursor.workspace;
    let repositories = app
        .topology
        .workspaces
        .iter()
        .filter_map(|workspace| workspace.git.as_ref().map(|git| git.repo_key.clone()))
        .collect::<Vec<_>>();
    for (workspace_index, workspace) in app.topology.workspaces.iter().enumerate() {
        let selection = (workspace_index == selected_workspace)
            .then_some(app.topology_cursor.selection[workspace_index]);
        let mut rendered =
            topology_lines_selected(workspace, width, selection, &app.theme, &repositories);
        if workspace.id == app.previous_workspace_id.as_deref().unwrap_or("") {
            rendered[0].spans[0] = Span::styled("  ←  ", Style::default().fg(app.theme.red));
        }
        for (line_index, mut line) in rendered.into_iter().enumerate() {
            let mut style = if workspace_index == selected_workspace {
                Style::default()
                    .bg(app.theme.selection_background(false))
                    .fg(app.theme.text)
            } else {
                Style::default().fg(app.theme.text)
            };
            if workspace_index == selected_workspace
                && ((app.topology_cursor.depth == TopologyDepth::Workspace && line_index == 0)
                    || (app.topology_cursor.depth == TopologyDepth::Tab && line_index == 1)
                    || (app.topology_cursor.depth == TopologyDepth::Pane && line_index == 2))
            {
                style = Style::default()
                    .bg(app.theme.selection_background(true))
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD);
            }
            line = line.style(style);
            lines.push(ListItem::new(line));
        }
    }
    if lines.is_empty() {
        lines.push(ListItem::new(Line::styled(
            "no open workspaces",
            Style::default().fg(app.theme.overlay0),
        )));
    }
    let item_heights: Vec<u16> = lines.iter().map(|item| item.height() as u16).collect();
    let selected_item = if app.topology.workspaces.is_empty() {
        None
    } else {
        Some(selected_workspace * 4)
    };
    let mut state = ListState::default();
    state.select(selected_item);
    let list = List::new(lines).block(block);
    f.render_stateful_widget(list, area, &mut state);
    let mut hits = Vec::new();
    let mut topology_hits = Vec::new();
    let mut y = list_area.y;
    for (index, height) in item_heights.into_iter().enumerate() {
        if y.saturating_add(height) > list_area.bottom() {
            break;
        }
        if index % 4 == 0 && index / 4 < app.topology.workspaces.len() {
            let workspace = index / 4;
            let workspace_node = &app.topology.workspaces[workspace];
            hits.push((y..y + height.saturating_add(3), workspace));
            let selected_tab = if workspace == selected_workspace {
                app.topology_cursor.selection[workspace].tab
            } else {
                workspace_node
                    .tabs
                    .iter()
                    .position(|tab| tab.focused)
                    .unwrap_or(0)
            };
            for line in 0..4 {
                let ranges = if line == 1 || line == 2 {
                    let selected_child = if workspace == selected_workspace {
                        Some(if line == 1 {
                            app.topology_cursor.selection[workspace].tab
                        } else {
                            app.topology_cursor.selection[workspace].pane
                        })
                    } else {
                        None
                    };
                    topology_child_ranges(
                        workspace_node,
                        line,
                        selected_tab,
                        selected_child,
                        width,
                        list_area.x + 9,
                    )
                } else {
                    Vec::new()
                };
                if ranges.is_empty() {
                    topology_hits.push(TopologyHit {
                        y: y + line as u16..y + line as u16 + 1,
                        x: list_area.x..list_area.right(),
                        workspace,
                        line,
                        tab: selected_tab,
                        child: None,
                    });
                } else {
                    for (x, child) in ranges {
                        topology_hits.push(TopologyHit {
                            y: y + line as u16..y + line as u16 + 1,
                            x,
                            workspace,
                            line,
                            tab: selected_tab,
                            child: Some(child),
                        });
                    }
                }
            }
        }
        y += height;
    }
    ListHits {
        area: list_area,
        rows: hits,
        topology: topology_hits,
    }
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) -> ListHits {
    if app.topology_view() {
        return draw_topology_list(f, app, area);
    }
    let row_width = area.width.saturating_sub(3) as usize;
    let source_width = source_column_width(app);
    let detailed_layout = detailed_layout(app, row_width, source_width);
    let mut items = Vec::new();
    let mut item_entries = Vec::new();
    let mut selected_row = None;
    for (row, idx) in app.filtered.iter().enumerate() {
        let e = &app.entries[*idx];
        let color = source_color(&app.theme, &e.source);

        if row == app.selected {
            selected_row = Some(items.len());
        }
        {
            let status = entry_status(e).filter(|status| *status != "unknown");
            let status_label = status
                .map(|status| truncate_end(status, detailed_layout.status_width.saturating_sub(2)));
            let status_text = status_label
                .as_deref()
                .map(|status| {
                    format!(
                        "{} {status}",
                        status_icon_at(&e.source, status, app.spinner_tick)
                    )
                })
                .unwrap_or_default();
            let status_text_width = Span::raw(&status_text).width();
            let topology_entry = is_topology_entry(e);
            let raw_path = e.path.display().to_string();
            let raw_metadata = if topology_entry || e.source == Source::Agent {
                String::new()
            } else {
                entry_metadata(e)
            };
            let show_metadata = !matches!(e.source, Source::Zoxide | Source::Root)
                && detailed_layout.right_width > 0
                && !raw_metadata.is_empty()
                && raw_metadata != raw_path;
            let metadata = if show_metadata {
                truncate_breadcrumb(&raw_metadata, detailed_layout.right_width)
            } else {
                String::new()
            };
            let metadata_width = Span::raw(&metadata).width();
            let path_width = row_width
                .saturating_sub(source_width.saturating_add(1))
                .saturating_sub(detailed_layout.status_width)
                .saturating_sub(detailed_layout.title_width)
                .saturating_sub(2)
                .saturating_sub(detailed_layout.right_width)
                .saturating_sub(usize::from(detailed_layout.right_width > 0));
            let destination_width = detailed_layout
                .title_width
                .saturating_add(2)
                .saturating_add(path_width);
            let title = truncate_end(display_title(e), detailed_layout.title_width);
            let path = truncate_end(&display_path(e), path_width);
            let destination = if topology_entry {
                truncate_breadcrumb(&e.subtitle, destination_width)
            } else {
                title.clone()
            };
            let status_color = status
                .map(|status| agent_status_color(&app.theme, status))
                .unwrap_or(color);
            let mut title_spans = source_spans(app, e, source_width);
            title_spans.extend([
                Span::styled(status_text, Style::default().fg(status_color)),
                Span::raw(
                    " ".repeat(
                        detailed_layout
                            .status_width
                            .saturating_sub(status_text_width),
                    ),
                ),
            ]);
            if topology_entry {
                let destination_width_used = Span::raw(&destination).width();
                title_spans.extend(breadcrumb_spans(app, e, &destination));
                title_spans.push(Span::raw(
                    " ".repeat(destination_width.saturating_sub(destination_width_used)),
                ));
            } else {
                title_spans.extend([
                    Span::styled(title.clone(), Style::default().fg(app.theme.text)),
                    Span::raw(
                        " ".repeat(
                            detailed_layout
                                .title_width
                                .saturating_sub(title.chars().count()),
                        ),
                    ),
                    Span::raw("  "),
                    Span::styled(path.clone(), Style::default().fg(app.theme.subtext0)),
                    Span::raw(" ".repeat(path_width.saturating_sub(path.chars().count()))),
                ]);
            }
            if detailed_layout.right_width > 0 {
                title_spans.push(Span::raw(" "));
                if !metadata.is_empty() {
                    title_spans.extend(breadcrumb_spans(app, e, &metadata));
                }
                title_spans.push(Span::raw(
                    " ".repeat(detailed_layout.right_width.saturating_sub(metadata_width)),
                ));
            }
            items.push(ListItem::new(Line::from(title_spans)));
        }
        item_entries.push(Some(row));
    }
    if items.is_empty() {
        items.push(ListItem::new(Line::styled(
            "no destination matches that query",
            Style::default().fg(app.theme.overlay0),
        )));
        item_entries.push(None);
    }
    let item_heights: Vec<u16> = items.iter().map(|item| item.height() as u16).collect();
    let mut state = ListState::default();
    state.select(selected_row);
    let block = Block::default()
        .title(format!(" RESULTS · {} ", app.filtered.len()))
        .borders(Borders::RIGHT);
    let list_area = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(app.theme.selection_background(true)))
        .highlight_symbol("→ ");
    f.render_stateful_widget(list, area, &mut state);

    let mut rows = Vec::new();
    let mut y = list_area.y;
    for (entry, height) in item_entries
        .into_iter()
        .zip(item_heights)
        .skip(state.offset())
    {
        if y.saturating_add(height) > list_area.bottom() {
            break;
        }
        if let Some(entry) = entry {
            rows.push((y..y + height, entry));
        }
        y += height;
    }
    ListHits {
        area: list_area,
        rows,
        topology: Vec::new(),
    }
}

fn source_color(theme: &Theme, source: &Source) -> Color {
    match source {
        Source::Workspace => theme.green,
        Source::Project => theme.mauve,
        Source::Zoxide => theme.blue,
        Source::Root => theme.teal,
        Source::Agent => theme.yellow,
        Source::Server => theme.green,
        Source::Session => theme.green,
        Source::QuickAction => theme.mauve,
        Source::Integration => theme.red,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;

    use super::*;
    use crate::{
        config::Config,
        model::EntryAction,
        theme::Theme,
        topology::{OpenTopology, PaneNode, TabNode, TopologyCursor},
    };

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn popup_open_errors_return_failure_instead_of_success() {
        let error = open_action_error(
            "Herdr API error (ui_busy): popup already open".into(),
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Herdr API error (ui_busy): popup already open"
        );
    }

    #[test]
    fn projected_topology_rows_use_breadcrumb_destinations_without_cwd_columns() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let mut workspace = entry(Source::Workspace, "old-workspace");
        workspace.source_label = Some("workspace".into());
        workspace.subtitle = "repo › main".into();
        workspace.path = PathBuf::from("/old/workspace-cwd");
        workspace.action = EntryAction::FocusWorkspace {
            session: Some("session".into()),
            id: "w1".into(),
        };
        workspace.search_terms.push("repo-key:/repo/.git".into());

        let mut tab = entry(Source::Workspace, "old-tab");
        tab.source_label = Some("tab".into());
        tab.subtitle = "repo › main › Tab".into();
        tab.path = PathBuf::from("/old/tab-cwd");
        tab.action = EntryAction::FocusTab {
            session: Some("session".into()),
            id: "w1:t1".into(),
        };
        tab.search_terms.push("repo-key:/repo/.git".into());

        let mut pane = entry(Source::Agent, "old-pane");
        pane.source_label = Some("pane".into());
        pane.subtitle = "repo › main › Tab › Pane".into();
        pane.path = PathBuf::from("/old/pane-cwd");
        pane.action = EntryAction::FocusPane {
            session: Some("session".into()),
            id: "w1:p1".into(),
        };
        pane.search_terms.push("repo-key:/repo/.git".into());
        pane.search_terms.push("agent-status:done".into());

        app.entries = vec![workspace, tab, pane];
        app.filtered = (0..app.entries.len()).collect();
        app.filtered_scores = vec![0; app.entries.len()];
        app.selected = usize::MAX;
        app.query = "repo".into();

        let backend = TestBackend::new(120, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let buffer = terminal.backend().buffer();
        let repository_color = repository_color("/repo/.git", &["/repo/.git".into()], &app.theme);
        let rows = [
            ("workspace", "repo › main"),
            ("tab", "repo › main › Tab"),
            ("pane", "repo › main › Tab › Pane"),
        ];
        let mut destination_column = None;
        for (source, destination) in rows {
            let (row_index, line) = text
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains(destination))
                .unwrap();
            assert!(!line.contains("old-"));
            assert!(!line.contains("/old/"));
            assert_eq!(line.find(source), Some(0));
            let destination_start = line.find(destination).unwrap();
            let column = Span::raw(&line[..destination_start]).width();
            if let Some(expected) = destination_column {
                assert_eq!(column, expected);
            } else {
                destination_column = Some(column);
            }
            let repository_start = line.find("repo").unwrap() as u16;
            let repository_cell = &buffer[(repository_start, row_index as u16)];
            assert_eq!(repository_cell.fg, repository_color);
            assert!(repository_cell.modifier.contains(Modifier::BOLD));
            assert!(!buffer[(repository_start + 5, row_index as u16)]
                .modifier
                .contains(Modifier::BOLD));
        }
        assert!(text.contains("✓ done"));
    }

    #[test]
    fn ctrl_w_filters_to_workspaces_tabs_and_panes() {
        let mut app = App::new(Config::default(), Theme::terminal());

        assert!(matches!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)
            ),
            Action::Continue
        ));
        assert_eq!(app.source_filter, Some(Source::Workspace));
        assert!(app.topology_view());
    }

    #[test]
    fn topology_help_names_workspaces_tabs_and_panes() {
        let app = App::new(Config::default(), Theme::terminal());
        let binding = keybindings(&app)
            .into_iter()
            .find(|binding| binding.command == Command::Filter(Source::Workspace))
            .unwrap();

        assert_eq!(binding.label, "Workspaces / Tabs / Panes");
        assert_eq!(
            binding.compact_hint(&app).unwrap().1,
            "Workspaces / Tabs / Panes"
        );
        assert!(!binding.label.contains("open"));
        assert!(!binding.label.contains("topology"));
    }

    #[test]
    fn collapse_and_expand_keep_topology_depth_navigation() {
        let mut app = topology_test_app();
        app.topology_cursor.enter_tab(&app.topology);
        assert_eq!(app.topology_cursor.depth, TopologyDepth::Tab);

        execute_command(&mut app, Command::Collapse, key(KeyCode::Left));
        assert_eq!(app.topology_cursor.depth, TopologyDepth::Workspace);
        execute_command(&mut app, Command::Expand, key(KeyCode::Right));
        assert_eq!(app.topology_cursor.depth, TopologyDepth::Tab);
    }

    #[test]
    fn topology_navigation_restores_workspace_child_memory() {
        let topology = OpenTopology {
            workspaces: vec![WorkspaceNode {
                id: "w1".into(),
                label: "Workspace".into(),
                session: Some("session".into()),
                focused: true,
                tabs: vec![TabNode {
                    id: "t1".into(),
                    label: "Tab".into(),
                    focused: true,
                    panes: vec![PaneNode {
                        id: "p1".into(),
                        label: "Pane".into(),
                        cwd: PathBuf::new(),
                        focused: true,
                        title: None,
                        agent: None,
                    }],
                }],
                git: None,
            }],
        };
        let mut cursor = TopologyCursor::new(&topology);
        cursor.enter_tab(&topology);
        cursor.enter_pane(&topology);
        assert_eq!(cursor.depth, TopologyDepth::Pane);
        cursor.leave_to_workspace();
        cursor.enter_tab(&topology);
        assert_eq!(cursor.depth, TopologyDepth::Tab);
    }

    fn entry(source: Source, title: &str) -> Entry {
        Entry {
            source,
            title: title.into(),
            subtitle: String::new(),
            path: PathBuf::from(title),
            workspace_id: None,
            workspace_label: None,
            agent_target: None,
            project: None,
            action: EntryAction::FocusOrCreateDir,
            source_label: None,
            search_terms: vec![],
        }
    }

    #[test]
    fn selected_flat_rows_preserve_semantic_text_styling() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let mut workspace = entry(Source::Workspace, "Destination");
        workspace.source_label = Some("workspace".into());
        workspace.subtitle = "repo › main".into();
        workspace.search_terms.push("repo-key:/repo/.git".into());
        app.entries = vec![workspace];
        app.filtered = vec![0];
        app.filtered_scores = vec![0];
        app.selected = 0;
        app.query = "repo".into();

        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let buffer = terminal.backend().buffer();
        let (row, line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("repo › main"))
            .unwrap();
        let repository_x = line.find("repo").unwrap() as u16;
        let main_x = line.find("main").unwrap() as u16;

        assert_eq!(
            buffer[(repository_x, row as u16)].fg,
            repository_color("/repo/.git", &["/repo/.git".into()], &app.theme)
        );
        assert!(buffer[(repository_x, row as u16)]
            .modifier
            .contains(Modifier::BOLD));
        assert!(!buffer[(main_x, row as u16)]
            .modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn flat_rows_put_status_before_aligned_destinations() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let mut idle = entry(Source::Agent, "first-destination");
        idle.search_terms.push("agent-status:idle".into());
        let mut blocked = entry(Source::Agent, "second-destination");
        blocked.search_terms.push("agent-status:blocked".into());
        let mut working = entry(Source::Agent, "third-destination");
        working.search_terms.push("agent-status:working".into());
        let mut ordinary = entry(Source::Workspace, "ordinary");
        ordinary.subtitle = "remaining metadata".into();
        app.entries = vec![idle, blocked, working, ordinary];
        app.filtered = (0..app.entries.len()).collect();
        app.filtered_scores = vec![0; app.entries.len()];
        app.selected = usize::MAX;
        app.query = "status".into();

        let backend = TestBackend::new(100, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let buffer = terminal.backend().buffer();
        let expected = [
            ("first-destination", "○", "idle", app.theme.green),
            ("second-destination", "!", "blocked", app.theme.red),
            ("third-destination", "⠋", "working", app.theme.yellow),
        ];

        for (destination, symbol, status, color) in expected {
            let (row, line) = text
                .lines()
                .enumerate()
                .find(|(_, line)| line.contains(destination))
                .unwrap();
            assert_eq!(line.matches(status).count(), 1);
            assert_eq!(line.chars().take(5).collect::<String>(), "agent");
            assert_eq!(line.chars().nth(9), symbol.chars().next());
            assert_eq!(
                line.chars().skip(11).take(status.len()).collect::<String>(),
                status
            );
            assert_eq!(line.chars().nth(18), destination.chars().next());
            assert_eq!(buffer[(9, row as u16)].fg, color);
            assert_eq!(buffer[(11, row as u16)].fg, color);
        }

        let (_row, ordinary_line) = text
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("ordinary"))
            .unwrap();
        assert!(ordinary_line.chars().skip(9).take(9).all(|c| c == ' '));
        assert_eq!(ordinary_line.chars().nth(18), Some('o'));
        assert!(ordinary_line.contains("remaining metadata"));
    }

    fn topology_test_app() -> App {
        let topology = OpenTopology {
            workspaces: vec![WorkspaceNode {
                id: "w1".into(),
                label: "Workspace".into(),
                session: Some("s".into()),
                focused: true,
                tabs: vec![TabNode {
                    id: "t1".into(),
                    label: "Tab".into(),
                    focused: true,
                    panes: vec![
                        PaneNode {
                            id: "p1".into(),
                            label: "One".into(),
                            cwd: PathBuf::new(),
                            focused: true,
                            title: None,
                            agent: None,
                        },
                        PaneNode {
                            id: "p2".into(),
                            label: "Two".into(),
                            cwd: PathBuf::new(),
                            focused: false,
                            title: None,
                            agent: None,
                        },
                    ],
                }],
                git: None,
            }],
        };
        let mut app = App::new(Config::default(), Theme::terminal());
        app.topology_entries = crate::topology::query_entries(&topology, false);
        app.topology = topology;
        app.topology_cursor = TopologyCursor::new(&app.topology);
        app
    }

    #[test]
    fn vertical_navigation_crosses_workspace_boundaries() {
        let mut app = topology_test_app();
        let mut other = app.topology.workspaces[0].clone();
        other.id = "w2".into();
        other.label = "Other".into();
        app.topology.workspaces.push(other);
        app.topology_cursor.depth = TopologyDepth::Pane;

        handle_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.topology_cursor.workspace, 1);
        assert_eq!(app.topology_cursor.depth, TopologyDepth::Pane);

        app.topology_cursor.depth = TopologyDepth::Workspace;
        handle_key(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.topology_cursor.workspace, 0);
    }

    #[test]
    fn topology_mouse_selects_and_remembers_exact_pane_span() {
        let mut app = topology_test_app();
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = ListHits::default();
        terminal
            .draw(|frame| hits = draw_list(frame, &app, frame.area()))
            .unwrap();
        let hit = hits
            .topology
            .iter()
            .find(|hit| hit.line == 2 && hit.child == Some(1))
            .unwrap();
        let action = handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.x.start,
                row: hit.y.start,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert!(matches!(action, Action::Continue));
        assert_eq!(app.topology_cursor.depth, TopologyDepth::Pane);
        assert_eq!(app.topology_cursor.selection[0].pane, 1);
        app.sync_topology_cursor();
        assert_eq!(app.topology_cursor.selection[0].pane, 1);
    }

    #[test]
    fn topology_mouse_keeps_non_selected_workspace_tab_context() {
        let mut app = topology_test_app();
        app.topology.workspaces.push(WorkspaceNode {
            id: "w2".into(),
            label: "Other".into(),
            session: Some("s".into()),
            focused: false,
            tabs: vec![
                TabNode {
                    id: "t2a".into(),
                    label: "First".into(),
                    focused: false,
                    panes: vec![PaneNode {
                        id: "p2a".into(),
                        label: "Wrong".into(),
                        cwd: PathBuf::new(),
                        focused: false,
                        title: None,
                        agent: None,
                    }],
                },
                TabNode {
                    id: "t2b".into(),
                    label: "Focused".into(),
                    focused: true,
                    panes: vec![PaneNode {
                        id: "p2b".into(),
                        label: "Right".into(),
                        cwd: PathBuf::new(),
                        focused: true,
                        title: None,
                        agent: None,
                    }],
                },
            ],
            git: None,
        });
        app.topology_entries = crate::topology::query_entries(&app.topology, false);
        app.topology_cursor = TopologyCursor::new(&app.topology);
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = ListHits::default();
        terminal
            .draw(|frame| hits = draw_list(frame, &app, frame.area()))
            .unwrap();
        let hit = hits
            .topology
            .iter()
            .find(|hit| hit.workspace == 1 && hit.line == 2 && hit.child == Some(0))
            .unwrap();
        let _ = handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.x.start,
                row: hit.y.start,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert_eq!(app.topology_cursor.selection[1].tab, 1);
        assert_eq!(app.topology_cursor.selection[1].pane, 0);
    }

    #[test]
    fn topology_rows_reserve_selection_slots_for_tabs_and_panes() {
        let mut app = topology_test_app();
        app.topology.workspaces[0].tabs.push(TabNode {
            id: "t2".into(),
            label: "Other".into(),
            focused: false,
            panes: vec![PaneNode {
                id: "p3".into(),
                label: "Three".into(),
                cwd: PathBuf::new(),
                focused: false,
                title: None,
                agent: None,
            }],
        });
        let rows = crate::topology::topology_rows_selected(
            &app.topology.workspaces[0],
            80,
            Some(crate::topology::ChildSelection { tab: 0, pane: 1 }),
        );

        assert!(rows[1].contains("[Tab]  Other"));
        assert!(rows[2].contains(" One  [Two] "));
    }

    #[test]
    fn topology_rows_keep_exact_terminal_widths() {
        let app = topology_test_app();
        for width in [44, 60, 110] {
            let rows = crate::topology::topology_rows_selected(
                &app.topology.workspaces[0],
                width,
                Some(app.topology_cursor.selection[0]),
            );
            assert!(rows.iter().all(|row| Span::raw(row).width() == width));
            assert!(rows.iter().all(|row| !row.contains('\n')));
        }
    }

    #[test]
    fn topology_mouse_hits_selected_child_when_value_exceeds_span() {
        let mut app = topology_test_app();
        app.topology.workspaces[0].tabs[0].label = "A selected tab label 123456789012".into();
        let backend = TestBackend::new(44, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = ListHits::default();
        terminal
            .draw(|frame| hits = draw_list(frame, &app, frame.area()))
            .unwrap();
        assert!(hits
            .topology
            .iter()
            .any(|hit| hit.workspace == 0 && hit.line == 1 && hit.child == Some(0)));
    }

    #[test]
    fn topology_rows_keep_selected_far_right_child_visible() {
        let mut app = topology_test_app();
        for index in 0..10 {
            app.topology.workspaces[0].tabs.push(TabNode {
                id: format!("t{index}"),
                label: format!("Tab-{index}"),
                focused: false,
                panes: vec![PaneNode {
                    id: format!("p{index}"),
                    label: format!("Pane-{index}"),
                    cwd: PathBuf::new(),
                    focused: false,
                    title: None,
                    agent: None,
                }],
            });
        }
        app.topology_cursor.selection[0].tab = 10;
        app.topology_cursor.selection[0].pane = 0;
        let rows = crate::topology::topology_rows_selected(
            &app.topology.workspaces[0],
            44,
            Some(app.topology_cursor.selection[0]),
        );
        assert!(rows[1].contains("[Tab-9]") || rows[1].contains("[Tab-10]"));
        assert!(rows[2].contains("[Pane-9]") || rows[2].contains("[Pane-10]"));
    }

    #[test]
    fn topology_cursor_clamps_to_deepest_existing_target() {
        let mut app = topology_test_app();
        app.topology.workspaces[0].tabs[0].panes.clear();
        app.topology_cursor.depth = TopologyDepth::Pane;
        app.topology_cursor.clamp(&app.topology);
        assert_eq!(app.topology_cursor.depth, TopologyDepth::Tab);
        app.topology.workspaces[0].tabs.clear();
        app.topology_cursor.clamp(&app.topology);
        assert_eq!(app.topology_cursor.depth, TopologyDepth::Workspace);
    }

    #[test]
    fn topology_actions_keep_exact_workspace_tab_and_pane_ids() {
        let mut app = topology_test_app();
        assert!(
            matches!(app.topology_selected_entry().unwrap().action, EntryAction::FocusWorkspace { ref id, .. } if id == "w1")
        );
        app.topology_cursor.enter_tab(&app.topology);
        assert!(
            matches!(app.topology_selected_entry().unwrap().action, EntryAction::FocusTab { ref id, .. } if id == "t1")
        );
        app.topology_cursor.enter_pane(&app.topology);
        assert!(
            matches!(app.topology_selected_entry().unwrap().action, EntryAction::FocusPane { ref id, .. } if id == "p1")
        );
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn herdr_plus_sources_share_color() {
        let theme = Theme::terminal();
        assert_eq!(
            source_color(&theme, &Source::Project),
            source_color(&theme, &Source::QuickAction)
        );
    }

    #[test]
    fn alt_enter_opens_selected_directory_with_configured_template() {
        let mut config = Config::default();
        config.picker.directory_template = Some("default.toml".into());
        let mut app = App::new(config, Theme::terminal());
        app.entries = vec![entry(Source::Zoxide, "/tmp")];
        app.apply_filter();

        assert!(matches!(
            handle_key(&mut app, key(KeyCode::Enter)),
            Action::Open
        ));
        assert!(matches!(
            handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            Action::OpenTemplate
        ));

        app.config.picker.directory_template_key = "ctrl-g".into();
        assert!(matches!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)
            ),
            Action::OpenTemplate
        ));
    }

    #[test]
    fn update_badge_renders_action_and_f5_triggers_it() {
        let mut app = App::new(Config::default(), Theme::terminal());
        assert!(matches!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::F(5), crossterm::event::KeyModifiers::NONE)
            ),
            Action::Continue
        ));
        app.update_available = Some("0.3.2".into());

        let backend = TestBackend::new(70, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();

        assert!(buffer_text(&terminal).contains("↑ v0.3.2 available · F5 update"));
        assert!(matches!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::F(5), crossterm::event::KeyModifiers::NONE)
            ),
            Action::Update
        ));
    }

    #[test]
    fn status_colors_match_herdr() {
        let theme = Theme::terminal();

        assert_eq!(agent_status_color(&theme, "blocked"), theme.red);
        assert_eq!(agent_status_color(&theme, "working"), theme.yellow);
        assert_eq!(agent_status_color(&theme, "done"), theme.teal);
        assert_eq!(agent_status_color(&theme, "idle"), theme.green);
        assert_eq!(agent_status_color(&theme, "unknown"), theme.overlay0);
    }

    #[test]
    fn normal_mode_hides_query_bar_until_search_starts() {
        let mut app = App::new(Config::default(), Theme::terminal());
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let normal_text = buffer_text(&terminal);
        assert!(!normal_text.contains("query"));
        assert!(!normal_text.contains("filter:all"));

        handle_key(&mut app, key(KeyCode::Char('/')));
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let search_text = buffer_text(&terminal);
        assert!(search_text.contains("query"));
        assert!(search_text.contains("filter:all"));
    }

    #[test]
    fn draw_uses_the_picker_title() {
        let app = App::new(Config::default(), Theme::terminal());
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();

        assert!(buffer_text(&terminal).contains("Helm"));
    }

    #[test]
    fn topology_visual_columns_and_partial_styles_match_design() {
        let mut app = topology_test_app();
        app.topology.workspaces[0].git = Some(crate::topology::GitIdentity {
            repo_key: "/repo/.git".into(),
            label: "repo".into(),
            head: crate::topology::GitHead::Branch("main".into()),
        });
        app.topology_cursor.depth = TopologyDepth::Tab;
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(9, 2)].symbol(), "t");
        assert_eq!(buffer[(9, 3)].symbol(), "p");
        assert_eq!(buffer[(9, 4)].symbol(), "p");
        let repository_x = buffer_text(&terminal)
            .lines()
            .nth(1)
            .unwrap()
            .find("repo")
            .unwrap() as u16;
        assert_eq!(buffer[(repository_x, 1)].fg, app.theme.teal);
        assert!(buffer[(repository_x, 1)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(repository_x + 6, 1)]
            .modifier
            .contains(Modifier::BOLD));

        let mut flat = App::new(Config::default(), Theme::terminal());
        let mut result = entry(Source::Workspace, "Destination");
        result.source_label = Some("open".into());
        result.subtitle = "repo › main › Tab".into();
        result.search_terms.push("repo-key:/repo/.git".into());
        flat.entries = vec![result];
        flat.query = "repo".into();
        flat.apply_filter();
        flat.selected = usize::MAX;
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &flat, f.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer_text(&terminal);
        let (row, line) = rendered
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("Destination"))
            .unwrap();
        let line = line.to_string();
        let source_x = line.find("open").unwrap() as u16;
        let parent_x = line.find("repo").unwrap() as u16;
        assert_eq!(buffer[(source_x, row as u16)].fg, flat.theme.overlay0);
        assert!(!buffer[(source_x, row as u16)]
            .modifier
            .contains(Modifier::BOLD));
        assert_eq!(buffer[(parent_x, row as u16)].fg, flat.theme.teal);
        assert!(buffer[(parent_x, row as u16)]
            .modifier
            .contains(Modifier::BOLD));
        assert!(!buffer[(parent_x + 7, row as u16)]
            .modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn selected_topology_uses_terminal_selection_colors() {
        let app = topology_test_app();
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(0, 1)].bg, Color::DarkGray);
        assert_eq!(buffer[(0, 2)].bg, Color::DarkGray);
    }

    #[test]
    fn selected_depth_is_stronger_when_surface_colors_match() {
        let mut app = topology_test_app();
        app.theme.surface1 = app.theme.surface0;
        app.topology_cursor.depth = TopologyDepth::Tab;
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(0, 1)].bg, app.theme.selection_background(false));
        assert_eq!(buffer[(0, 2)].bg, app.theme.selection_background(true));
        assert!(buffer[(0, 2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(9, 2)].symbol(), "t");
    }

    #[test]
    fn modified_chords_never_insert_text() {
        let mut app = App::new(Config::default(), Theme::terminal());

        // Ctrl-Backspace reaches the app as Ctrl-H on most terminals; it used to
        // fall through and type an "h".
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.query, "");

        // Any other unbound chord is inert too.
        for code in ['d', 'f', 'k'] {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL),
            );
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(code), KeyModifiers::ALT),
            );
        }
        assert_eq!(app.query, "");

        // Plain and shifted characters are text in Search mode.
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
        );
        assert_eq!(app.query, "aB");
    }

    #[test]
    fn ctrl_backspace_deletes_a_word_in_both_encodings() {
        for chord in [
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
        ] {
            let mut app = App::new(Config::default(), Theme::terminal());
            app.query = "foo bar".into();

            handle_key(&mut app, chord);
            assert_eq!(app.query, "foo ");

            handle_key(&mut app, chord);
            assert_eq!(app.query, "");

            // Empty query stays empty rather than panicking.
            handle_key(&mut app, chord);
            assert_eq!(app.query, "");
        }
    }

    #[test]
    fn delete_word_handles_trailing_space_and_multibyte() {
        let mut app = App::new(Config::default(), Theme::terminal());

        app.query = "foo bar  ".into();
        app.delete_query_word();
        assert_eq!(app.query, "foo ");

        app.query = "café naïve".into();
        app.delete_query_word();
        assert_eq!(app.query, "café ");

        app.query = "solo".into();
        app.delete_query_word();
        assert_eq!(app.query, "");
    }

    #[test]
    fn normal_navigation_aliases_move_and_query_editing_keeps_hjkl() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.entries = vec![entry(Source::Root, "one"), entry(Source::Root, "two")];
        app.source_filter = Some(Source::Root);
        app.apply_filter();
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.selected, 1);
        handle_key(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('h')));
        handle_key(&mut app, key(KeyCode::Char('j')));
        handle_key(&mut app, key(KeyCode::Char('l')));
        handle_key(&mut app, key(KeyCode::Char('k')));
        assert_eq!(app.query, "hjlk");
    }

    #[test]
    fn question_mark_toggles_registry_help_overlay() {
        let mut app = App::new(Config::default(), Theme::terminal());
        handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Help);

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains(" Keybindings "));
        assert!(text.contains("agents"));
        assert!(!text.contains("?/?"));

        handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn registry_maps_ctrl_b_to_mark_without_stealing_enter() {
        let app = App::new(Config::default(), Theme::terminal());
        let mark = keybindings(&app)
            .into_iter()
            .find(|binding| binding.command == Command::ToggleMark)
            .unwrap();

        assert!(mark.label.contains("mark"));
        assert!(mark.matches(
            &app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)
        ));
        assert!(!mark.matches(&app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(keybindings(&app)
            .into_iter()
            .find(|binding| binding.command == Command::Open)
            .unwrap()
            .matches(&app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[test]
    fn disabled_sources_are_absent_from_filter_bindings_and_footer() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.config.sources.servers = false;
        app.config.sources.sessions = false;

        let bindings = keybindings(&app);
        assert!(!bindings.iter().any(|binding| {
            matches!(
                binding.command,
                Command::Filter(Source::Server) | Command::Filter(Source::Session)
            )
        }));

        let backend = TestBackend::new(110, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(!text.contains("server"));
        assert!(!text.contains("session"));
    }

    #[test]
    fn compact_footer_groups_movement_and_lists_filters() {
        let app = App::new(Config::default(), Theme::terminal());
        let backend = TestBackend::new(110, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("↓/j up/down"));
        assert!(text.contains("⌃A agent"));
        assert!(text.contains("⌃Z zoxide"));
        assert!(!text.contains("k move up"));
    }

    #[test]
    fn mouse_ignores_input_outside_results() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.entries = vec![
            entry(Source::Workspace, "one"),
            entry(Source::Workspace, "two"),
        ];
        app.filtered = vec![0, 1];
        let hits = ListHits {
            area: Rect::new(0, 3, 40, 10),
            rows: vec![(4..5, 0), (5..6, 1)],
            topology: Vec::new(),
        };

        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 50,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn uppercase_j_and_k_switch_workspaces_at_nested_depth() {
        let mut app = topology_test_app();
        let mut other = app.topology.workspaces[0].clone();
        other.id = "w2".into();
        other.label = "Other".into();
        app.topology.workspaces.push(other);
        app.topology_cursor.depth = TopologyDepth::Pane;

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
        );
        assert_eq!(app.topology_cursor.workspace, 1);

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT),
        );
        assert_eq!(app.topology_cursor.workspace, 0);
    }

    #[test]
    fn normal_mode_letters_wait_for_the_search_trigger() {
        let mut app = App::new(Config::default(), Theme::terminal());

        handle_key(&mut app, key(KeyCode::Char('x')));
        assert_eq!(app.query, "");

        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('x')));
        assert_eq!(app.query, "x");
        assert_eq!(app.input_mode, InputMode::Search);
    }

    #[test]
    fn input_modes_transition_exclusively() {
        let mut app = App::new(Config::default(), Theme::terminal());
        assert_eq!(app.input_mode, InputMode::Normal);

        handle_key(&mut app, key(KeyCode::Char('/')));
        assert_eq!(app.input_mode, InputMode::Search);

        handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Help);

        handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
    }
}
