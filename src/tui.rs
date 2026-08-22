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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::{
    app::{App, InputMode},
    keymap::{keybindings, Command},
    model::{Entry, EntryAction, OpenNode, Source},
    paths::home,
    sources::status_icon_at,
    theme::Theme,
};

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
                    eprintln!("{e}");
                    wait_for_key();
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
}

fn handle_mouse(app: &mut App, mouse: MouseEvent, hits: &ListHits) -> Action {
    if app.input_mode == InputMode::Help || !hits.area.contains((mouse.column, mouse.row).into()) {
        return Action::Continue;
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => app.prev(),
        MouseEventKind::ScrollDown => app.next(),
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

    if app.config.picker.vim_mode && app.input_mode == InputMode::Normal {
        return Action::Continue;
    }

    // Only plain and shifted characters are text. Without this guard every
    // unbound chord inserts its letter -- Ctrl-Backspace arrives as Ctrl-H on
    // most terminals and used to type an "h".
    if let KeyCode::Char(c) = key.code {
        if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            app.query.push(c);
            app.apply_filter();
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
        Command::Open => Action::Open,
        Command::OpenTemplate => Action::OpenTemplate,
        Command::Update => Action::Update,
        Command::MoveUp => {
            app.prev();
            Action::Continue
        }
        Command::MoveDown => {
            app.next();
            Action::Continue
        }
        Command::Collapse => {
            app.collapse_selected();
            Action::Continue
        }
        Command::Expand => {
            app.expand_selected();
            Action::Continue
        }
        Command::StartSearch => {
            app.query.clear();
            app.apply_filter();
            app.input_mode = InputMode::Search;
            Action::Continue
        }
        Command::CycleFilter => {
            app.cycle_filter();
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
        Command::TogglePreview => {
            app.preview = !app.preview;
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
                app.input_mode = if app.config.picker.vim_filter_search {
                    InputMode::Search
                } else {
                    InputMode::Normal
                };
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
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

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
            .style(Style::default().bg(app.theme.panel_bg))
            .borders(Borders::BOTTOM),
    );
    f.render_widget(search, rows[0]);

    let body = if app.preview {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(rows[1])
    };

    let list_hits = draw_list(f, app, body[0]);
    if app.preview {
        draw_preview(f, app, body[1]);
    }

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
    match entry.source {
        Source::Agent => Some(
            entry
                .subtitle
                .split_once(" · ")
                .map(|(status, _)| status)
                .filter(|status| !status.is_empty())
                .unwrap_or("unknown"),
        ),
        Source::Workspace => entry.subtitle.strip_prefix("agent:").map(|rest| {
            rest.split_once(" · ")
                .map(|(status, _)| status)
                .unwrap_or(rest)
        }),
        _ => None,
    }
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

fn source_column_width(app: &App) -> usize {
    app.entries
        .iter()
        .map(|entry| Span::raw(truncate_terminal(entry.source_name(), 10)).width())
        .max()
        .unwrap_or(0)
}

fn source_spans(app: &App, entry: &Entry, width: usize) -> Vec<Span<'static>> {
    let source = truncate_terminal(entry.source_name(), 10);
    let source_width = Span::raw(&source).width();
    vec![
        Span::styled(
            source,
            Style::default()
                .fg(source_color(&app.theme, &entry.source))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(width.saturating_sub(source_width).saturating_add(1))),
    ]
}

fn topology_session_key(name: Option<&str>) -> String {
    name.unwrap_or("<default>").to_string()
}

fn topology_workspace_key(session: Option<&str>, workspace_id: &str) -> String {
    format!("{}::{workspace_id}", topology_session_key(session))
}

fn topology_session_node_key(session: Option<&str>) -> String {
    format!("session::{}", topology_session_key(session))
}

fn topology_workspace_node_key(session: Option<&str>, workspace_id: &str) -> String {
    format!(
        "workspace::{}",
        topology_workspace_key(session, workspace_id)
    )
}

fn topology_parent_key(entry: &Entry) -> Option<String> {
    match entry.open_node.as_ref()? {
        OpenNode::Workspace {
            session,
            parent_workspace_id,
            ..
        } => Some(match parent_workspace_id.as_deref() {
            Some(parent_id) => topology_workspace_node_key(session.as_deref(), parent_id),
            None => topology_session_node_key(session.as_deref()),
        }),
        OpenNode::Tab {
            session,
            workspace_id,
            ..
        } => Some(topology_workspace_node_key(
            session.as_deref(),
            workspace_id,
        )),
    }
}

fn topology_row_is_last(app: &App, row: usize, entry: &Entry) -> bool {
    let Some(parent_key) = topology_parent_key(entry) else {
        return true;
    };
    !app.filtered.iter().skip(row + 1).any(|index| {
        topology_parent_key(&app.entries[*index]).as_deref() == Some(parent_key.as_str())
    })
}

fn topology_workspace_entry_index(
    app: &App,
    session: Option<&str>,
    workspace_id: &str,
) -> Option<usize> {
    app.entries.iter().position(|entry| {
        entry.workspace_id.as_deref() == Some(workspace_id)
            && matches!(
                entry.open_node.as_ref(),
                Some(OpenNode::Workspace { session: candidate, .. })
                    if candidate.as_deref() == session
            )
    })
}

fn topology_workspace_ancestors(app: &App, entry: &Entry) -> Vec<usize> {
    let (session, mut workspace_id) = match entry.open_node.as_ref() {
        Some(OpenNode::Workspace {
            session,
            parent_workspace_id,
            ..
        }) => (session.as_deref(), parent_workspace_id.as_deref()),
        Some(OpenNode::Tab {
            session,
            workspace_id,
            ..
        }) => (session.as_deref(), Some(workspace_id.as_str())),
        _ => return Vec::new(),
    };
    let mut ancestors = Vec::new();
    while let Some(id) = workspace_id {
        let Some(index) = topology_workspace_entry_index(app, session, id) else {
            break;
        };
        ancestors.push(index);
        workspace_id = match app.entries[index].open_node.as_ref() {
            Some(OpenNode::Workspace {
                parent_workspace_id,
                ..
            }) => parent_workspace_id.as_deref(),
            _ => None,
        };
    }
    ancestors.reverse();
    ancestors
}

fn topology_branch_prefix(app: &App, entry: &Entry, row: usize) -> String {
    let mut prefix = String::from("    ");
    for ancestor_index in topology_workspace_ancestors(app, entry) {
        let continuation = app
            .filtered
            .iter()
            .position(|candidate| *candidate == ancestor_index)
            .is_some_and(|ancestor_row| {
                !topology_row_is_last(app, ancestor_row, &app.entries[ancestor_index])
            });
        prefix.push_str(if continuation { "│   " } else { "    " });
    }
    prefix.push_str(if topology_row_is_last(app, row, entry) {
        "└─ "
    } else {
        "├─ "
    });
    prefix
}

fn flat_branch_prefix(app: &App, entry: &Entry, row: usize) -> String {
    match entry.open_node.as_ref() {
        Some(OpenNode::Workspace { .. }) => "    ".into(),
        Some(OpenNode::Tab { .. }) => {
            let last = topology_row_is_last(app, row, entry);
            format!("      {}", if last { "└─ " } else { "├─ " })
        }
        _ => topology_branch_prefix(app, entry, row),
    }
}

#[derive(Clone, Copy)]
struct DetailedLayout {
    prefix_width: usize,
    title_width: usize,
    marker_width: usize,
    right_width: usize,
}

fn detailed_layout(app: &App, row_width: usize, source_width: usize) -> DetailedLayout {
    let minimum_prefix_width = if app.entries.iter().any(|entry| entry.open_node.is_some()) {
        11
    } else {
        8
    };
    let prefix_width = app
        .entries
        .iter()
        .filter(|entry| entry.open_node.is_some())
        .map(|entry| {
            11usize.saturating_add(
                4usize.saturating_mul(topology_workspace_ancestors(app, entry).len()),
            )
        })
        .max()
        .unwrap_or(minimum_prefix_width)
        .max(minimum_prefix_width);
    let marker_width = if row_width >= 80 { 10 } else { 4 };
    let state_width = app
        .entries
        .iter()
        .filter_map(|entry| {
            (entry.open_node.is_none())
                .then(|| entry_status(entry))
                .flatten()
                .filter(|status| *status != "unknown")
                .map(|status| Span::raw(status).width())
        })
        .max()
        .unwrap_or(0);
    let source_budget = source_width.saturating_add(1);
    let right_width = metadata_width(row_width.saturating_add(3) as u16)
        .max(state_width)
        .min(
            row_width.saturating_sub(
                source_budget
                    .saturating_add(prefix_width)
                    .saturating_add(marker_width)
                    .saturating_add(4),
            ),
        );
    let right_separator = usize::from(right_width > 0);
    let content_width = row_width
        .saturating_sub(source_budget)
        .saturating_sub(prefix_width)
        .saturating_sub(marker_width)
        .saturating_sub(right_width)
        .saturating_sub(right_separator);
    let title_width = content_width.min(48) / 2;
    DetailedLayout {
        prefix_width,
        title_width,
        marker_width,
        right_width,
    }
}

fn worktree_label(row_width: usize) -> &'static str {
    if row_width >= 80 {
        "WORKTREE"
    } else {
        "WT"
    }
}

fn open_entry_line(app: &App, entry: &Entry, row: usize, row_width: usize) -> Line<'static> {
    let node = entry.open_node.as_ref().expect("open topology entry");
    let searching = !app.query.trim().is_empty();
    let (prefix, prefix_color, marker, marker_color) = match node {
        OpenNode::Workspace {
            session, focused, ..
        } => {
            let expanded = searching
                || entry.workspace_id.as_deref().is_some_and(|id| {
                    app.expanded_workspaces
                        .contains(&topology_workspace_key(session.as_deref(), id))
                });
            let marker_color = if app.is_pinned(entry) {
                app.theme.yellow
            } else if *focused {
                app.theme.accent
            } else if app.config.jump_back.pin_previous
                && app.query.trim().is_empty()
                && app.source_filter.is_none()
                && matches!(entry.action, EntryAction::FocusWorkspace { .. })
                && entry.workspace_id == app.previous_workspace_id
            {
                app.theme.red
            } else {
                app.theme.overlay0
            };
            (
                format!(
                    "{}{} ",
                    if searching {
                        topology_branch_prefix(app, entry, row)
                    } else {
                        flat_branch_prefix(app, entry, row)
                    },
                    if expanded { '▾' } else { '▸' }
                ),
                app.theme.overlay0,
                if app.is_pinned(entry) || *focused || marker_color == app.theme.red {
                    "◆ ".to_string()
                } else {
                    "  ".to_string()
                },
                marker_color,
            )
        }
        OpenNode::Tab { focused, .. } => (
            if searching {
                topology_branch_prefix(app, entry, row)
            } else {
                flat_branch_prefix(app, entry, row)
            },
            app.theme.overlay0,
            if *focused { "● " } else { "  " }.to_string(),
            app.theme.green,
        ),
    };

    let source_width = source_column_width(app);
    let layout = detailed_layout(app, row_width, source_width);
    let raw_prefix_width = prefix.chars().count() + marker.chars().count();
    let prefix_width = if app.config.picker.detailed_rows {
        layout.prefix_width
    } else {
        raw_prefix_width
    };
    let prefix_padding = " ".repeat(prefix_width.saturating_sub(raw_prefix_width));
    let is_workspace = matches!(node, OpenNode::Workspace { .. });
    let linked_worktree = matches!(
        node,
        OpenNode::Workspace {
            linked_worktree: true,
            ..
        }
    );
    let kind_column = if app.config.picker.detailed_rows {
        if linked_worktree {
            format!("  {}", worktree_label(row_width))
        } else {
            " ".repeat(layout.marker_width)
        }
    } else if linked_worktree {
        "⎇ ".to_string()
    } else {
        String::new()
    };
    let kind_width = kind_column.chars().count();
    let raw_path = if app.config.picker.detailed_rows && is_workspace {
        display_path(entry)
    } else {
        String::new()
    };
    let source_budget = source_width.saturating_add(1);
    let fixed_width = source_budget.saturating_add(prefix_width);
    let content_budget =
        row_width
            .saturating_sub(fixed_width)
            .saturating_sub(if app.config.picker.detailed_rows {
                layout.right_width + usize::from(layout.right_width > 0)
            } else {
                0
            });
    let show_path = !raw_path.is_empty()
        && content_budget
            > layout
                .title_width
                .saturating_add(kind_width)
                .saturating_add(2);
    let title_budget = if app.config.picker.detailed_rows {
        layout.title_width
    } else if show_path {
        content_budget
            .saturating_sub(kind_width)
            .saturating_sub(2)
            .clamp(1, 32)
    } else {
        content_budget.saturating_sub(kind_width).max(1)
    };
    let title = truncate_end(display_title(entry), title_budget);
    let title_len = title.chars().count();
    let title_column_width = if app.config.picker.detailed_rows || is_workspace {
        title_budget
    } else {
        title.chars().count()
    };
    let path_budget = content_budget
        .saturating_sub(title_column_width)
        .saturating_sub(kind_width)
        .saturating_sub(usize::from(show_path) * 2);
    let path = show_path.then(|| truncate_end(&raw_path, path_budget));
    let path_text = path
        .filter(|path| !path.is_empty())
        .map(|path| format!("  {path}"))
        .unwrap_or_default();
    let occupied = fixed_width + title_column_width + kind_width + path_text.chars().count();
    let spacer = " ".repeat(row_width.saturating_sub(occupied));
    let mut spans = source_spans(app, entry, source_width);
    spans.extend([
        Span::styled(prefix, Style::default().fg(prefix_color)),
        Span::styled(marker, Style::default().fg(marker_color)),
        Span::raw(prefix_padding),
        Span::styled(title, Style::default().fg(app.theme.text)),
        Span::raw(" ".repeat(title_column_width.saturating_sub(title_len))),
        Span::styled(
            kind_column,
            Style::default()
                .fg(if linked_worktree {
                    app.theme.teal
                } else {
                    app.theme.overlay0
                })
                .add_modifier(if linked_worktree && app.config.picker.detailed_rows {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::styled(path_text, Style::default().fg(app.theme.subtext0)),
        Span::raw(spacer),
    ]);
    Line::from(spans)
}

fn entry_branch(app: &App, entry: &Entry, _group_end: bool) -> (&'static str, Color) {
    let is_workspace = entry.source == Source::Workspace;
    let is_current = is_workspace && entry.search_terms.iter().any(|term| term == "focused");
    let is_previous = is_workspace
        && app.config.jump_back.pin_previous
        && app.query.trim().is_empty()
        && app.source_filter.is_none()
        && entry.workspace_id.is_some()
        && entry.workspace_id == app.previous_workspace_id;
    if app.is_pinned(entry) {
        ("  ◆  ", app.theme.yellow)
    } else if is_current {
        ("  ◆  ", app.theme.accent)
    } else if is_previous {
        ("  ◆  ", app.theme.red)
    } else {
        ("     ", app.theme.overlay0)
    }
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) -> ListHits {
    let show_scores = !app.query.trim().is_empty();
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
        let (branch, branch_color) = entry_branch(app, e, false);
        let score = show_scores
            .then(|| app.filtered_scores.get(row).map(|s| format!("score {s}")))
            .flatten();

        if e.open_node.is_some() {
            items.push(ListItem::new(open_entry_line(app, e, row, row_width)));
        } else if app.config.picker.detailed_rows {
            let status = entry_status(e);
            let icon = status
                .map(|status| format!("{}  ", status_icon_at(&e.source, status, app.spinner_tick)))
                .unwrap_or_else(|| "   ".into());
            let status_label = status
                .filter(|status| *status != "unknown")
                .map(|status| truncate_end(status, detailed_layout.right_width));
            let raw_path = e.path.display().to_string();
            let raw_metadata = if e.source == Source::Agent {
                String::new()
            } else {
                entry_metadata(e)
            };
            let show_metadata = !matches!(e.source, Source::Zoxide | Source::Root)
                && detailed_layout.right_width > 0
                && !raw_metadata.is_empty()
                && raw_metadata != raw_path;
            let separator_width = usize::from(show_metadata && status_label.is_some()) * 3;
            let status_width = status_label
                .as_deref()
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0);
            let metadata_budget = detailed_layout
                .right_width
                .saturating_sub(status_width)
                .saturating_sub(separator_width);
            let metadata = if show_metadata {
                truncate_end(&raw_metadata, metadata_budget)
            } else {
                String::new()
            };
            let right_width = status_width
                + usize::from(!metadata.is_empty() && status_label.is_some()) * 3
                + metadata.chars().count();
            let raw_prefix_width = branch.chars().count() + icon.chars().count();
            let prefix_padding = " ".repeat(
                detailed_layout
                    .prefix_width
                    .saturating_sub(raw_prefix_width),
            );
            let path_width = row_width
                .saturating_sub(source_width.saturating_add(1))
                .saturating_sub(detailed_layout.prefix_width)
                .saturating_sub(detailed_layout.title_width)
                .saturating_sub(detailed_layout.marker_width)
                .saturating_sub(2)
                .saturating_sub(detailed_layout.right_width)
                .saturating_sub(usize::from(detailed_layout.right_width > 0));
            let title = truncate_end(display_title(e), detailed_layout.title_width);
            let path = truncate_end(&display_path(e), path_width);
            let status_color = status
                .map(|status| agent_status_color(&app.theme, status))
                .unwrap_or(color);
            let mut title_spans = source_spans(app, e, source_width);
            title_spans.extend([
                Span::styled(branch, Style::default().fg(branch_color)),
                Span::styled(icon, Style::default().fg(status_color)),
                Span::raw(prefix_padding),
                Span::styled(title.clone(), Style::default().fg(app.theme.text)),
                Span::raw(
                    " ".repeat(
                        detailed_layout
                            .title_width
                            .saturating_sub(title.chars().count()),
                    ),
                ),
                Span::raw(" ".repeat(detailed_layout.marker_width)),
                Span::raw("  "),
                Span::styled(path.clone(), Style::default().fg(app.theme.subtext0)),
                Span::raw(" ".repeat(path_width.saturating_sub(path.chars().count()))),
            ]);
            if detailed_layout.right_width > 0 {
                title_spans.push(Span::raw(" "));
                if let Some(status_label) = status_label {
                    title_spans.push(Span::styled(
                        status_label.to_string(),
                        Style::default().fg(status_color),
                    ));
                    if !metadata.is_empty() {
                        title_spans
                            .push(Span::styled(" · ", Style::default().fg(app.theme.overlay0)));
                    }
                }
                if !metadata.is_empty() {
                    title_spans.push(Span::styled(
                        metadata,
                        Style::default().fg(app.theme.overlay0),
                    ));
                }
                title_spans.push(Span::raw(
                    " ".repeat(detailed_layout.right_width.saturating_sub(right_width)),
                ));
            }
            items.push(ListItem::new(Line::from(title_spans)));
        } else {
            let status_text = entry_status(e);
            let status = status_text
                .map(|status| format!("{} ", status_icon_at(&e.source, status, app.spinner_tick)))
                .unwrap_or_default();
            let subtitle = if e.subtitle.is_empty() {
                String::new()
            } else {
                format!("  {}", e.subtitle)
            };
            let left_len = source_width.saturating_add(1)
                + branch.chars().count()
                + status.chars().count()
                + e.title.chars().count()
                + subtitle.chars().count();
            let spacer = score
                .as_ref()
                .map(|score| {
                    " ".repeat(
                        row_width
                            .saturating_sub(left_len + score.chars().count())
                            .max(2),
                    )
                })
                .unwrap_or_default();
            let mut spans = source_spans(app, e, source_width);
            spans.extend([
                Span::styled(branch, Style::default().fg(branch_color)),
                Span::styled(
                    status,
                    Style::default().fg(status_text
                        .map(|status| agent_status_color(&app.theme, status))
                        .unwrap_or(color)),
                ),
                Span::styled(e.title.clone(), Style::default().fg(app.theme.text)),
                Span::styled(subtitle, Style::default().fg(app.theme.subtext0)),
            ]);
            if let Some(score) = score {
                spans.push(Span::raw(spacer));
                spans.push(Span::styled(score, Style::default().fg(app.theme.overlay0)));
            }
            items.push(ListItem::new(Line::from(spans)));
        }
        item_entries.push(Some(row));
    }
    let item_heights: Vec<u16> = items.iter().map(|item| item.height() as u16).collect();
    let mut state = ListState::default();
    state.select(selected_row);
    let block = Block::default().title(" Results ").borders(Borders::RIGHT);
    let list_area = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(app.theme.surface0)
                .add_modifier(Modifier::BOLD),
        )
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
    }
}

fn draw_preview(f: &mut Frame, app: &App, area: Rect) {
    let text = if let Some(e) = app.selected_entry() {
        preview_text(app, e)
    } else {
        "No results".into()
    };
    let p = Paragraph::new(text)
        .style(Style::default().fg(app.theme.text))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .title(" Preview ")
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(app.theme.surface_dim)),
        );
    f.render_widget(p, area);
}

fn preview_text(app: &App, e: &Entry) -> String {
    let entry_type = e
        .open_node
        .as_ref()
        .map(OpenNode::kind_label)
        .unwrap_or_else(|| e.source_name());
    let mut lines = vec![format!("type: {entry_type}"), format!("title: {}", e.title)];
    if let Some(node) = &e.open_node {
        lines.push(format!("session: {}", node.session().unwrap_or("default")));
    }
    if !e.path.as_os_str().is_empty() {
        lines.push(format!("path: {}", e.path.display()));
    }
    if !e.subtitle.is_empty() {
        lines.push(format!("info: {}", e.subtitle));
    }
    if let Some(label) = &e.workspace_label {
        lines.push(format!("workspace: {label}"));
    }
    if let Some(id) = &e.workspace_id {
        lines.push(format!("workspace_id: {id}"));
    }
    if let Some(target) = &e.agent_target {
        lines.push(format!("agent target: {target}"));
    }
    if e.source == Source::Agent {
        lines.push(
            "agent filters: @ all agents (configured sort), !agent, @workspace/status, /path"
                .into(),
        );
    }
    if !e.search_terms.is_empty() {
        lines.push(format!("search terms: {}", e.search_terms.join(", ")));
    }
    let workspaces = app.workspaces_for_entry(e);
    if !workspaces.is_empty() {
        lines.push("existing workspaces:".into());
        for ws in workspaces {
            lines.push(format!(
                "  - {} [{}] tabs:{} panes:{} {}",
                ws.id,
                ws.label,
                ws.tab_count,
                ws.pane_count,
                ws.path.display()
            ));
        }
    }
    if let Some(p) = &e.project {
        lines.push("".into());
        lines.push("project tabs:".into());
        for tab in &p.tabs {
            let cmd = tab.command.as_deref().unwrap_or("shell");
            lines.push(format!("  - {}: {}", tab.name, cmd));
        }
    }
    lines.push("".into());
    let action: &str = match &e.action {
        EntryAction::FocusWorkspace { .. } => "focus existing workspace",
        EntryAction::FocusTab { .. } => "focus exact tab",
        EntryAction::FocusAgent { .. } => "focus agent pane",
        EntryAction::FocusPane { .. } => "focus exact pane",
        EntryAction::OpenRemote { .. } => "open remote Herdr",
        EntryAction::InvokePluginAction { .. } => "invoke Herdr plugin action",
        EntryAction::RunCommand { .. } if e.source == Source::Session => "open session via plugin",
        EntryAction::RunCommand { .. } => "run integration command",
        EntryAction::OpenProject if app.matching_project_workspace(e).is_some() => {
            "focus matching project workspace"
        }
        EntryAction::OpenProject => "create project workspace + tabs",
        EntryAction::FocusOrCreateDir if app.matching_dir_workspace(e).is_some() => {
            "focus matching dir workspace"
        }
        EntryAction::FocusOrCreateDir => "create dir workspace",
    };
    lines.push(format!("enter: {action}"));
    if let Some(template) = app.directory_template_for_selected() {
        lines.push(format!(
            "{}: apply template {template}",
            app.config.picker.directory_template_key
        ));
    }
    lines.join("\n")
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
    use crate::{config::Config, theme::Theme};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
            open_node: None,
        }
    }

    fn topology_app(query: &str) -> App {
        let mut workspace = entry(Source::Workspace, "Project");
        workspace.path = PathBuf::from("/tmp/project");
        workspace.workspace_id = Some("w1".into());
        workspace.action = EntryAction::FocusWorkspace {
            session: Some("work".into()),
            id: "w1".into(),
        };
        workspace.open_node = Some(OpenNode::Workspace {
            session: Some("work".into()),
            parent_workspace_id: None,
            linked_worktree: false,
            focused: true,
            tab_count: 2,
            pane_count: 3,
        });

        let mut code = entry(Source::Workspace, "Code");
        code.workspace_id = Some("w1".into());
        code.workspace_label = Some("Project".into());
        code.action = EntryAction::FocusTab {
            session: Some("work".into()),
            id: "w1:t1".into(),
        };
        code.open_node = Some(OpenNode::Tab {
            session: Some("work".into()),
            workspace_id: "w1".into(),
            focused: true,
            pane_count: 1,
        });

        let mut server = entry(Source::Workspace, "Server");
        server.workspace_id = Some("w1".into());
        server.workspace_label = Some("Project".into());
        server.action = EntryAction::FocusTab {
            session: Some("work".into()),
            id: "w1:t2".into(),
        };
        server.open_node = Some(OpenNode::Tab {
            session: Some("work".into()),
            workspace_id: "w1".into(),
            focused: false,
            pane_count: 2,
        });

        let mut agent = entry(Source::Agent, "claude · Project");
        agent.subtitle = "idle · w1:p1 · w1:t1".into();
        let mut app = App::new(Config::default(), Theme::load(false));
        app.entries = vec![workspace, code, server, agent];
        app.query = query.into();
        app.apply_filter();
        app
    }

    fn worktree_topology_app() -> App {
        let base = topology_app("");
        let parent = base.entries[0].clone();
        let parent_code = base.entries[1].clone();
        let parent_server = base.entries[2].clone();
        let agent = base.entries[3].clone();

        let mut child_a = parent.clone();
        child_a.title = "Feature A".into();
        child_a.path = PathBuf::from("/tmp/feature-a");
        child_a.workspace_id = Some("child-a".into());
        child_a.workspace_label = Some("Feature A".into());
        child_a.action = EntryAction::FocusWorkspace {
            session: Some("work".into()),
            id: "child-a".into(),
        };
        child_a.open_node = Some(OpenNode::Workspace {
            session: Some("work".into()),
            parent_workspace_id: Some("w1".into()),
            linked_worktree: true,
            focused: false,
            tab_count: 1,
            pane_count: 1,
        });
        let mut child_a_tab = parent_code.clone();
        child_a_tab.title = "Child A Tab".into();
        child_a_tab.workspace_id = Some("child-a".into());
        child_a_tab.workspace_label = Some("Feature A".into());
        child_a_tab.action = EntryAction::FocusTab {
            session: Some("work".into()),
            id: "child-a:t1".into(),
        };
        child_a_tab.open_node = Some(OpenNode::Tab {
            session: Some("work".into()),
            workspace_id: "child-a".into(),
            focused: false,
            pane_count: 1,
        });

        let mut child_b = child_a.clone();
        child_b.title = "Feature B".into();
        child_b.workspace_id = Some("child-b".into());
        child_b.workspace_label = Some("Feature B".into());
        child_b.action = EntryAction::FocusWorkspace {
            session: Some("work".into()),
            id: "child-b".into(),
        };
        let mut child_b_tab = child_a_tab.clone();
        child_b_tab.title = "Child B Tab".into();
        child_b_tab.workspace_id = Some("child-b".into());
        child_b_tab.workspace_label = Some("Feature B".into());
        child_b_tab.action = EntryAction::FocusTab {
            session: Some("work".into()),
            id: "child-b:t1".into(),
        };
        child_b_tab.open_node = Some(OpenNode::Tab {
            session: Some("work".into()),
            workspace_id: "child-b".into(),
            focused: false,
            pane_count: 1,
        });

        let mut app = App::new(Config::default(), Theme::load(false));
        app.entries = vec![
            parent,
            parent_code,
            parent_server,
            child_a,
            child_a_tab,
            child_b,
            child_b_tab,
            agent,
        ];
        app.apply_filter();
        app.expanded_workspaces.insert("work::child-a".into());
        app.expanded_workspaces.insert("work::child-b".into());
        app.apply_filter();
        app
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
        let theme = Theme::load(false);
        assert_eq!(
            source_color(&theme, &Source::Project),
            source_color(&theme, &Source::QuickAction)
        );
    }

    #[test]
    fn alt_enter_opens_selected_directory_with_configured_template() {
        let mut config = Config::default();
        config.picker.directory_template = Some("default.toml".into());
        let mut app = App::new(config, Theme::load(false));
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
        let mut app = App::new(Config::default(), Theme::load(false));
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
        let theme = Theme::load(false);

        assert_eq!(agent_status_color(&theme, "blocked"), theme.red);
        assert_eq!(agent_status_color(&theme, "working"), theme.yellow);
        assert_eq!(agent_status_color(&theme, "done"), theme.teal);
        assert_eq!(agent_status_color(&theme, "idle"), theme.green);
        assert_eq!(agent_status_color(&theme, "unknown"), theme.overlay0);
    }

    #[test]
    fn open_and_pinned_workspaces_replace_tree_branches_with_diamonds() {
        let mut app = App::new(Config::default(), Theme::load(false));
        let mut current = entry(Source::Workspace, "Current");
        current.workspace_id = Some("w1".into());
        current.search_terms.push("focused".into());
        let mut previous = entry(Source::Workspace, "Previous");
        previous.workspace_id = Some("w2".into());
        app.entries = vec![current, previous];
        app.filtered = vec![0, 1];
        app.filtered_scores = vec![0; 2];
        app.previous_workspace_id = Some("w2".into());
        app.selected = 1;

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let buffer = terminal.backend().buffer();

        assert!(text.contains("  ◆     Current"));
        assert!(text.contains("  ◆     Previous"));
        assert!(!text.contains("├─ ◆"));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.symbol() == "◆" && cell.fg == app.theme.accent));
        assert!(buffer
            .content()
            .iter()
            .any(|cell| cell.symbol() == "◆" && cell.fg == app.theme.red));
    }

    #[test]
    fn current_workspace_marker_wins_over_stale_pin() {
        let mut app = App::new(Config::default(), Theme::load(false));
        let mut current = entry(Source::Workspace, "Current");
        current.workspace_id = Some("w1".into());
        current.search_terms.push("focused".into());
        app.previous_workspace_id = Some("w1".into());

        assert_eq!(
            entry_branch(&app, &current, false),
            ("  ◆  ", app.theme.accent)
        );
    }

    #[test]
    fn marked_entry_uses_a_yellow_diamond() {
        let mut app = App::new(Config::default(), Theme::load(false));
        let marked = entry(Source::Root, "Marked");
        app.pinned_entries.insert("root:Marked".into());

        assert_eq!(
            entry_branch(&app, &marked, false),
            ("  ◆  ", app.theme.yellow)
        );
    }

    #[test]
    fn mark_marker_wins_over_previous_workspace() {
        let mut app = App::new(Config::default(), Theme::load(false));
        let mut previous = entry(Source::Workspace, "Previous");
        previous.workspace_id = Some("w2".into());
        previous.action = EntryAction::FocusWorkspace {
            session: None,
            id: "w2".into(),
        };
        app.previous_workspace_id = Some("w2".into());
        app.pinned_entries.insert("workspace:w2".into());

        assert_eq!(
            entry_branch(&app, &previous, false),
            ("  ◆  ", app.theme.yellow)
        );
    }

    #[test]
    fn mark_marker_wins_over_current_workspace() {
        let mut app = App::new(Config::default(), Theme::load(false));
        let mut current = entry(Source::Workspace, "Current");
        current.workspace_id = Some("w1".into());
        current.action = EntryAction::FocusWorkspace {
            session: None,
            id: "w1".into(),
        };
        current.search_terms.push("focused".into());
        app.pinned_entries.insert("workspace:w1".into());

        assert_eq!(
            entry_branch(&app, &current, false),
            ("  ◆  ", app.theme.yellow)
        );
    }

    #[test]
    fn draw_uses_the_host_pane_chrome() {
        let app = App::new(Config::default(), Theme::load(false));
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();

        assert!(!buffer_text(&terminal).contains("Helm"));
    }

    #[test]
    fn detailed_open_keeps_workspace_columns_stable_when_expanded() {
        let mut app = topology_app("");
        let collapsed = open_entry_line(&app, &app.entries[0], 0, 90).to_string();
        let collapsed_title = collapsed[..collapsed.find("Project").unwrap()]
            .chars()
            .count();

        app.expanded_workspaces.insert("work::w1".into());
        app.apply_filter();
        let expanded = open_entry_line(&app, &app.entries[0], 0, 90).to_string();
        let expanded_title = expanded[..expanded.find("Project").unwrap()]
            .chars()
            .count();

        assert_eq!(collapsed_title, expanded_title);
    }

    #[test]
    fn detailed_open_starts_with_workspace_without_session_banner() {
        let mut app = topology_app("");
        app.selected = usize::MAX;

        let backend = TestBackend::new(90, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();

        let text = buffer_text(&terminal);
        let workspace_y = text
            .lines()
            .position(|line| line.contains("Project"))
            .unwrap() as u16;
        let buffer = terminal.backend().buffer();

        assert!((0..89).all(|x| buffer[(x, workspace_y)].bg != app.theme.accent));
    }

    #[test]
    fn mixed_detailed_sections_share_columns_and_only_show_agent_state() {
        let mut app = topology_app("");
        if let Some(OpenNode::Workspace {
            linked_worktree, ..
        }) = app.entries[0].open_node.as_mut()
        {
            *linked_worktree = true;
        }
        app.entries[3].path = PathBuf::from("/tmp/agent");
        app.entries[3].subtitle = "working · w1:p2 · w1:t1".into();
        let mut root = entry(Source::Root, "Root project");
        root.path = PathBuf::from("/tmp/root");
        app.entries.push(root);
        app.apply_filter();
        app.selected = usize::MAX;

        let backend = TestBackend::new(110, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let workspace_line = text.lines().find(|line| line.contains("Project")).unwrap();
        let agent_line = text.lines().find(|line| line.contains("claude")).unwrap();
        let root_line = text
            .lines()
            .find(|line| line.contains("Root project"))
            .unwrap();

        assert!(workspace_line.contains("WORKTREE  /tmp/project"));
        assert!(agent_line.contains("working"));
        assert!(!text.contains("session · 1 workspaces"));
        assert!(!text.contains("workspace · 2 tabs · 3 panes"));
        assert!(!text.contains("w1:p2"));
        assert!(!text.contains("w1:t1"));

        let column = |line: &str, value: &str| line[..line.find(value).unwrap()].chars().count();
        assert_eq!(column(workspace_line, "Project"), 21);
        assert_eq!(column(agent_line, "claude"), 21);
        assert_eq!(column(root_line, "Root project"), 21);
        let workspace_path = column(workspace_line, "/tmp/project");
        assert_eq!(workspace_path, column(agent_line, "/tmp/agent"));
        assert_eq!(workspace_path, column(root_line, "/tmp/root"));
        assert_eq!(column(agent_line, "working"), 79);

        let marker_x = workspace_line[..workspace_line.find("WORKTREE").unwrap()]
            .chars()
            .count() as u16;
        let marker_y = text
            .lines()
            .position(|line| line.contains("WORKTREE"))
            .unwrap() as u16;
        let buffer = terminal.backend().buffer();
        assert!((marker_x..marker_x + "WORKTREE".len() as u16)
            .all(|x| buffer[(x, marker_y)].modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn narrow_detailed_sections_use_bold_wt_and_keep_paths_aligned() {
        let mut app = topology_app("");
        if let Some(OpenNode::Workspace {
            linked_worktree, ..
        }) = app.entries[0].open_node.as_mut()
        {
            *linked_worktree = true;
        }
        app.entries[0].path = PathBuf::from("/a");
        app.entries[3].title = "agent".into();
        app.entries[3].path = PathBuf::from("/b");
        app.entries[3].subtitle = "idle · w1:p1 · w1:t1".into();
        let mut root = entry(Source::Root, "root");
        root.path = PathBuf::from("/c");
        app.entries.push(root);
        app.apply_filter();
        app.selected = usize::MAX;

        let backend = TestBackend::new(50, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let workspace_line = text.lines().find(|line| line.contains("Project")).unwrap();
        let agent_line = text.lines().find(|line| line.contains("/b")).unwrap();
        let root_line = text.lines().find(|line| line.contains("/c")).unwrap();

        assert!(workspace_line.contains("WT  /a"));
        assert!(agent_line.contains("idle"));
        assert!(!text.contains("WORKTREE"));
        let column = |line: &str, value: &str| line[..line.rfind(value).unwrap()].chars().count();
        assert_eq!(column(workspace_line, "Project"), 21);
        assert_eq!(column(agent_line, "agent"), 21);
        assert_eq!(column(root_line, "root"), 21);
        let workspace_path = column(workspace_line, "/a");
        assert_eq!(workspace_path, column(agent_line, "/b"));
        assert_eq!(workspace_path, column(root_line, "/c"));
        assert_eq!(column(agent_line, "idle"), 43);

        let marker_x = workspace_line[..workspace_line.find("WT").unwrap()]
            .chars()
            .count() as u16;
        let marker_y = text.lines().position(|line| line.contains("WT")).unwrap() as u16;
        let buffer = terminal.backend().buffer();
        assert!((marker_x..marker_x + 2)
            .all(|x| buffer[(x, marker_y)].modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn detailed_rows_truncate_long_names_and_paths_without_wrapping() {
        let mut app = App::new(Config::default(), Theme::load(false));
        let mut root = entry(Source::Root, "very-long-directory-name");
        root.path = PathBuf::from("/projects/with/a/very/long/path");
        app.entries = vec![root];
        app.filtered = vec![0];
        app.filtered_scores = vec![0];
        app.selected = usize::MAX;

        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);

        let row = text.lines().find(|line| line.contains("very")).unwrap();
        assert!(row.matches('…').count() >= 2);
    }

    #[test]
    fn rendered_open_topology_starts_with_collapsed_flat_workspace_rows() {
        let app = topology_app("");
        let backend = TestBackend::new(110, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_list(frame, &app, frame.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);

        let open = text.find("open").unwrap();
        let workspace = text.find("Project").unwrap();
        let agent = text.find("agent").unwrap();
        assert!(open < workspace);
        assert!(workspace < agent);
        assert!(!text.contains("LIVE"));
        assert!(!text.contains(" ▾ open "));
        assert!(!text.contains("Code"));
        assert!(!text.contains("Server"));
        assert!(!text.contains("session · 1 workspaces"));
        assert!(!text.contains("workspace · 2 tabs · 3 panes"));
        assert!(!text.contains("tab · 1 pane"));
    }

    #[test]
    fn rendered_worktree_workspaces_are_flat_siblings_with_attached_tabs() {
        let app = worktree_topology_app();
        let backend = TestBackend::new(120, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_list(frame, &app, frame.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let parent = text.lines().find(|line| line.contains("Project")).unwrap();
        let child_a = text
            .lines()
            .find(|line| line.contains("Feature A"))
            .unwrap();
        let child_a_tab = text
            .lines()
            .find(|line| line.contains("Child A Tab"))
            .unwrap();
        let child_b = text
            .lines()
            .find(|line| line.contains("Feature B"))
            .unwrap();
        let child_b_tab = text
            .lines()
            .find(|line| line.contains("Child B Tab"))
            .unwrap();

        let column = |line: &str, label: &str| line[..line.find(label).unwrap()].chars().count();
        let path_column = |line: &str| {
            let byte = line.find("/tmp/").unwrap();
            line[..byte].chars().count()
        };
        assert_eq!(column(parent, "Project"), column(child_a, "Feature A"));
        assert_eq!(column(child_a, "Feature A"), column(child_b, "Feature B"));
        assert_eq!(path_column(parent), path_column(child_a));
        assert_eq!(path_column(child_a), path_column(child_b));
        assert!(child_a.contains("WORKTREE"));
        assert!(child_b.contains("WORKTREE"));
        assert_eq!(
            column(child_a_tab, "Child A Tab"),
            column(child_a, "Feature A")
        );
        assert_eq!(
            column(child_a_tab, "Child A Tab"),
            column(child_b_tab, "Child B Tab")
        );
        assert!(!text.contains("Code"));
        assert!(text.find("Project").unwrap() < text.find("Feature A").unwrap());
        assert!(text.find("Child A Tab").unwrap() < text.find("Feature B").unwrap());
        assert!(text.find("Feature B").unwrap() < text.find("Child B Tab").unwrap());
    }

    #[test]
    fn narrow_detailed_open_rows_budget_title_path_and_metadata() {
        let mut app = topology_app("");
        app.entries[0].title = "Extremely-long-workspace-title".into();
        app.entries[0].path = PathBuf::from("/projects/with/a/very/long/workspace/path");
        let backend = TestBackend::new(42, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_list(frame, &app, frame.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let workspace_line = text.lines().find(|line| line.contains('…')).unwrap();

        assert!(workspace_line.contains('…'));
        assert!(workspace_line.contains("open"));
        assert!(!workspace_line.contains("workspace · 2 tabs · 3 panes"));
        assert!(!workspace_line.contains("very/long/workspace/path"));
    }

    #[test]
    fn narrow_compact_worktree_rows_budget_extra_indentation() {
        let mut app = worktree_topology_app();
        app.config.picker.detailed_rows = false;
        app.entries[3].title = "Extremely-long-linked-worktree-title".into();
        let backend = TestBackend::new(42, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_list(frame, &app, frame.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let child_line = text
            .lines()
            .find(|line| line.contains("Extremely"))
            .unwrap();
        assert!(child_line.contains("…⎇ "));
        assert!(!child_line.contains("WT"));
        assert!(!child_line.contains("/tmp/feature-a"));

        let marker_x = child_line[..child_line.find('⎇').unwrap()].chars().count() as u16;
        let marker_y = text.lines().position(|line| line.contains('⎇')).unwrap() as u16;
        let marker = &terminal.backend().buffer()[(marker_x, marker_y)];
        assert_eq!(marker.fg, app.theme.teal);
        assert!(!marker.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn compact_open_rows_keep_topology_title_and_hide_workspace_path() {
        let mut app = topology_app("");
        app.config.picker.detailed_rows = false;
        app.entries[0].title = "Extremely-long-workspace-title".into();
        app.entries[0].path = PathBuf::from("/projects/with/a/very/long/workspace/path");
        let backend = TestBackend::new(42, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_list(frame, &app, frame.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);

        let workspace_line = text.lines().find(|line| line.contains("Extrem")).unwrap();
        assert!(workspace_line.contains('…'));
        assert!(workspace_line.contains("open"));
        assert!(!text.contains("/projects"));
    }

    #[test]
    fn unresolved_linked_worktree_keeps_its_marker_without_a_parent() {
        let app = topology_app("");
        let mut orphan = app.entries[0].clone();
        if let Some(OpenNode::Workspace {
            parent_workspace_id,
            linked_worktree,
            ..
        }) = orphan.open_node.as_mut()
        {
            *parent_workspace_id = None;
            *linked_worktree = true;
        }

        assert!(open_entry_line(&app, &orphan, 0, 80)
            .to_string()
            .contains("WORKTREE"));
    }

    #[test]
    fn topology_previous_marker_only_shows_on_initial_unfiltered_view() {
        let mut app = topology_app("");
        app.previous_workspace_id = Some("w1".into());
        let mut previous = app.entries[0].clone();
        previous.open_node = Some(OpenNode::Workspace {
            session: Some("work".into()),
            parent_workspace_id: None,
            linked_worktree: false,
            focused: false,
            tab_count: 2,
            pane_count: 3,
        });

        assert!(open_entry_line(&app, &previous, 0, 80)
            .to_string()
            .contains('◆'));

        app.query = "project".into();
        assert!(!open_entry_line(&app, &previous, 0, 80)
            .to_string()
            .contains('◆'));

        app.query.clear();
        app.source_filter = Some(Source::Workspace);
        assert!(!open_entry_line(&app, &previous, 0, 80)
            .to_string()
            .contains('◆'));
    }

    #[test]
    fn rendered_open_search_retains_ancestors_and_only_matching_tabs() {
        let app = topology_app("server");
        let backend = TestBackend::new(110, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_list(frame, &app, frame.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("Project"));
        assert!(text.contains("Server"));
        assert!(!text.contains("Code"));
        assert!(text.find("Project").unwrap() < text.find("Server").unwrap());
    }

    #[test]
    fn left_and_right_collapse_then_expand_open_workspace() {
        let mut app = topology_app("");
        app.selected = 0;
        execute_command(&mut app, Command::Collapse, key(KeyCode::Left));
        assert_eq!(app.filtered.len(), 2); // workspace + agent
        assert_eq!(app.selected_entry().unwrap().title, "Project");

        execute_command(&mut app, Command::Expand, key(KeyCode::Right));
        assert_eq!(app.filtered.len(), 4);
        assert_eq!(app.selected_entry().unwrap().title, "Code");
        execute_command(&mut app, Command::Collapse, key(KeyCode::Left));
        assert_eq!(app.selected_entry().unwrap().title, "Project");

        let mut app = topology_app("");
        assert!(!app.expanded_workspaces.contains("work::w1"));
        assert!(matches!(
            execute_command(&mut app, Command::Open, key(KeyCode::Enter)),
            Action::Open
        ));
        assert!(!app.expanded_workspaces.contains("work::w1"));
        assert_eq!(app.filtered.len(), 2);
        assert_eq!(app.selected_entry().unwrap().title, "Project");
    }

    #[test]
    fn list_renders_inline_source_rows_without_banners_or_outer_branches() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.entries = vec![
            entry(Source::Agent, "Claude"),
            entry(Source::Agent, "Codex"),
            entry(Source::Root, "Dotfiles"),
        ];
        app.filtered = vec![0, 1, 2];
        app.filtered_scores = vec![0; 3];
        app.selected = usize::MAX;

        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);

        assert_eq!(
            text.lines().filter(|line| line.contains("Claude")).count(),
            1
        );
        assert_eq!(
            text.lines().filter(|line| line.contains("Codex")).count(),
            1
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("Dotfiles"))
                .count(),
            1
        );
        assert_eq!(
            text.lines().filter(|line| line.contains("agent")).count(),
            2
        );
        assert_eq!(text.lines().filter(|line| line.contains("root")).count(), 1);
        assert!(!text.contains("LIVE"));
        assert!(!text.contains(" ▾ agent "));
        assert!(!text.contains("├─"));
        assert!(!text.contains("└─"));

        let buffer = terminal.backend().buffer();
        for (source, title) in [(Source::Agent, "Claude"), (Source::Root, "Dotfiles")] {
            let y = text.lines().position(|line| line.contains(title)).unwrap() as u16;
            let line = text.lines().nth(y as usize).unwrap();
            let x = line.find(source.label()).unwrap() as u16;
            for offset in 0..source.label().len() as u16 {
                assert_eq!(
                    buffer[(x + offset, y)].fg,
                    source_color(&app.theme, &source)
                );
            }
        }
    }

    #[test]
    fn source_labels_use_terminal_width_and_keep_later_columns_aligned() {
        let mut app = App::new(Config::default(), Theme::load(false));
        let mut integration = entry(Source::Integration, "Plugin item");
        integration.source_label = Some("整合性プラグイン".into());
        let root = entry(Source::Root, "Root item");
        app.entries = vec![integration, root];
        app.filtered = vec![0, 1];
        app.filtered_scores = vec![0; 2];
        app.selected = usize::MAX;

        let backend = TestBackend::new(70, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let integration_line = text
            .lines()
            .find(|line| line.contains("Plugin item"))
            .unwrap();
        let root_line = text
            .lines()
            .find(|line| line.contains("Root item"))
            .unwrap();
        let title_column =
            |line: &str, title: &str| line[..line.find(title).unwrap()].chars().count();

        assert!(integration_line.contains('…'));
        assert!(Span::raw(truncate_terminal("整合性プラグイン", 10)).width() <= 10);
        assert_eq!(
            title_column(integration_line, "Plugin item"),
            title_column(root_line, "Root item")
        );
    }

    #[test]
    fn open_rows_repeat_the_source_label_for_workspace_and_tabs() {
        let mut app = topology_app("");
        app.expanded_workspaces.insert("work::w1".into());
        app.apply_filter();
        app.selected = usize::MAX;

        let backend = TestBackend::new(90, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_list(f, &app, f.area());
            })
            .unwrap();
        let text = buffer_text(&terminal);

        assert_eq!(text.lines().filter(|line| line.contains("open")).count(), 3);
        assert!(!text.contains("LIVE"));
    }

    #[test]
    fn modified_chords_never_insert_text() {
        let mut app = App::new(Config::default(), Theme::load(false));

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

        // Plain and shifted characters are still text.
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
            let mut app = App::new(Config::default(), Theme::load(false));
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
        let mut app = App::new(Config::default(), Theme::load(false));

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
    fn vim_mode_uses_normal_keys_then_searches_with_slash() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.config.picker.vim_mode = true;
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert!(app.query.is_empty());

        handle_key(&mut app, key(KeyCode::Char('a')));
        assert_eq!(app.source_filter, Some(Source::Agent));

        handle_key(&mut app, key(KeyCode::Char('/')));
        assert_eq!(app.input_mode, InputMode::Search);
        assert_eq!(app.source_filter, Some(Source::Agent));

        handle_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.query, "j");

        handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn vim_filter_search_starts_search_after_source_key() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.config.picker.vim_mode = true;
        app.config.picker.vim_filter_search = true;

        handle_key(&mut app, key(KeyCode::Char('a')));
        assert_eq!(app.source_filter, Some(Source::Agent));
        assert_eq!(app.input_mode, InputMode::Search);

        handle_key(&mut app, key(KeyCode::Char('c')));
        assert_eq!(app.query, "c");
    }

    #[test]
    fn question_mark_toggles_registry_help_overlay() {
        let mut app = App::new(Config::default(), Theme::load(false));
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
        assert!(text.contains("toggle preview"));
        assert!(text.contains("agents"));
        assert!(!text.contains("?/?"));

        handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn registry_reports_active_toggle_state() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.preview = true;
        let preview = keybindings(&app)
            .into_iter()
            .find(|binding| binding.command == Command::TogglePreview)
            .unwrap();

        assert!(preview.is_active(&app));
    }

    #[test]
    fn registry_maps_ctrl_b_to_mark_without_stealing_enter() {
        let app = App::new(Config::default(), Theme::load(false));
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
        let mut app = App::new(Config::default(), Theme::load(false));
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
        let mut app = App::new(Config::default(), Theme::load(false));
        app.config.picker.vim_mode = true;
        let backend = TestBackend::new(110, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let text = buffer_text(&terminal);

        assert!(text.contains("j/k up/down"));
        assert!(text.contains("a agent"));
        assert!(text.contains("z zoxide"));
        assert!(!text.contains("k move up"));
    }

    #[test]
    fn rendered_mouse_hit_matches_the_visible_compact_row() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.config.picker.detailed_rows = false;
        app.entries = vec![
            entry(Source::Workspace, "one"),
            entry(Source::Workspace, "two"),
        ];
        app.filtered = vec![0, 1];
        app.selected = 1;

        let backend = TestBackend::new(50, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = ListHits::default();
        terminal.draw(|f| hits = draw(f, &app)).unwrap();
        let visible_row = buffer_text(&terminal)
            .lines()
            .position(|line| line.contains("one"))
            .expect("first result should be visible") as u16;

        assert!(matches!(
            handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: hits.area.x,
                    row: visible_row,
                    modifiers: KeyModifiers::NONE,
                },
                &hits,
            ),
            Action::Continue
        ));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn rendered_mouse_hits_follow_grouped_detailed_rows_after_scroll() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.config.picker.detailed_rows = true;
        app.entries = (0..8)
            .map(|index| entry(Source::Zoxide, &format!("/{index}")))
            .collect();
        app.filtered = vec![7, 2, 5, 0, 4, 1, 6, 3];
        app.selected = 7;

        let backend = TestBackend::new(50, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut hits = ListHits::default();
        terminal
            .draw(|f| hits = draw_list(f, &app, f.area()))
            .unwrap();
        let text = buffer_text(&terminal);
        assert_eq!(hits.rows.first().map(|(_, row)| *row), Some(3));
        assert!(!text.contains("/7"));
        let detail_row =
            text.lines()
                .position(|line| line.contains("/1"))
                .expect("filtered row should remain visible after scrolling") as u16;

        assert!(matches!(
            handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 1,
                    row: detail_row,
                    modifiers: KeyModifiers::NONE,
                },
                &hits,
            ),
            Action::Continue
        ));
        assert_eq!(app.selected, 5);
    }

    #[test]
    fn detailed_columns_stay_fixed_when_query_removes_agent_status() {
        let base = topology_app("");
        let mut child = base.entries[0].clone();
        child.title = "Child".into();
        child.path = PathBuf::from("/tmp/child");
        child.workspace_id = Some("child".into());
        child.open_node = Some(OpenNode::Workspace {
            session: Some("work".into()),
            parent_workspace_id: Some("w1".into()),
            linked_worktree: false,
            focused: false,
            tab_count: 0,
            pane_count: 0,
        });
        let mut agent = base.entries[3].clone();
        agent.subtitle = "working · child:p1 · child:t1".into();
        let mut app = App::new(Config::default(), Theme::load(false));
        app.entries = vec![base.entries[0].clone(), child, agent];
        app.apply_filter();
        app.selected = 0;
        let render = |app: &App| {
            let backend = TestBackend::new(50, 18);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    draw_list(frame, app, frame.area());
                })
                .unwrap();
            buffer_text(&terminal)
        };
        let unfiltered = render(&app);
        let unfiltered_project = unfiltered.lines().find(|line| line.contains('◆')).unwrap();
        let column = |line: &str, value: &str| line[..line.find(value).unwrap()].chars().count();
        let unfiltered_path = column(unfiltered_project, "/tm");

        app.query = "Child".into();
        app.source_filter = Some(Source::Workspace);
        app.apply_filter();
        app.selected = 0;
        let filtered = render(&app);
        assert!(!filtered.contains("working"));
        let filtered_project = filtered.lines().find(|line| line.contains('◆')).unwrap();
        assert_eq!(
            column(filtered_project, "Proj"),
            column(unfiltered_project, "Proj")
        );
        assert_eq!(column(filtered_project, "/tm"), unfiltered_path);

        let unfiltered_child = unfiltered
            .lines()
            .find(|line| line.contains("Child"))
            .unwrap();
        let filtered_child = filtered
            .lines()
            .find(|line| line.contains("Child"))
            .unwrap();
        assert_eq!(
            column(filtered_child, "Child"),
            column(unfiltered_child, "Child")
        );
        assert_eq!(
            column(unfiltered_child, "Child"),
            column(unfiltered_project, "Proj")
        );
        assert_eq!(
            column(filtered_child, "Child"),
            column(filtered_project, "Proj")
        );
        assert_eq!(
            column(filtered_child, "/tm"),
            column(unfiltered_child, "/tm")
        );
    }

    #[test]
    fn detailed_layout_budgets_deep_open_prefix() {
        let mut app = topology_app("");
        let mut child = app.entries[0].clone();
        child.title = "Child".into();
        child.workspace_id = Some("child".into());
        child.open_node = Some(OpenNode::Workspace {
            session: Some("work".into()),
            parent_workspace_id: Some("w1".into()),
            linked_worktree: false,
            focused: false,
            tab_count: 0,
            pane_count: 0,
        });
        app.entries = vec![app.entries[0].clone(), child];
        app.filtered = vec![0, 1];
        assert_eq!(
            detailed_layout(&app, 47, source_column_width(&app)).prefix_width,
            15
        );
    }

    #[test]
    fn mouse_scroll_moves_selection_inside_results() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.entries = vec![
            entry(Source::Workspace, "one"),
            entry(Source::Workspace, "two"),
        ];
        app.filtered = vec![0, 1];
        let hits = ListHits {
            area: Rect::new(0, 3, 40, 10),
            rows: vec![(4..5, 0), (5..6, 1)],
        };

        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 1,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert_eq!(app.selected, 1);

        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 1,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn mouse_click_selects_then_opens_result() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.entries = vec![
            entry(Source::Workspace, "one"),
            entry(Source::Workspace, "two"),
        ];
        app.filtered = vec![0, 1];
        let hits = ListHits {
            area: Rect::new(0, 3, 40, 10),
            rows: vec![(4..5, 0), (5..6, 1)],
        };
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };

        assert!(matches!(
            handle_mouse(&mut app, click, &hits),
            Action::Continue
        ));
        assert_eq!(app.selected, 1);
        assert!(matches!(handle_mouse(&mut app, click, &hits), Action::Open));
    }

    #[test]
    fn mouse_ignores_input_outside_results() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.entries = vec![
            entry(Source::Workspace, "one"),
            entry(Source::Workspace, "two"),
        ];
        app.filtered = vec![0, 1];
        let hits = ListHits {
            area: Rect::new(0, 3, 40, 10),
            rows: vec![(4..5, 0), (5..6, 1)],
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
    fn input_modes_transition_exclusively() {
        let mut app = App::new(Config::default(), Theme::load(false));
        app.config.picker.vim_mode = true;
        app.config.picker.vim_filter_search = true;
        assert_eq!(app.input_mode, InputMode::Normal);

        handle_key(&mut app, key(KeyCode::Char('a')));
        assert_eq!(app.input_mode, InputMode::Search);

        handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Help);

        handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
    }
}
