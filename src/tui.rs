use std::{
    io,
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
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
    activity::now_unix_secs,
    app::{load_workspaces, App, InputMode, SelectedWorkspace, Workspaces},
    config::Config,
    detail::{self, Change, Commit, GitDetail, Subagent},
    glance::{self, Group, Mark},
    keymap::{keybindings, Command},
    model::{Entry, Source},
    paths::home,
    sources::status_icon_at,
    theme::Theme,
    topology::{repository_color, AgentState, OpenTopology, PaneNode, WorkspaceNode},
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
    let mut detail_job: Option<(String, Receiver<GitDetail>)> = None;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;
    let mut list_hits = ListHits::default();
    let workspace_refresh = spawn_workspace_refresh(app.config.clone(), app.topology.clone());
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
        if let Ok(workspaces) = workspace_refresh.try_recv() {
            app.apply_workspaces(workspaces, now_unix_secs());
        }
        app.sync_detail_scroll();
        drive_detail(app, &mut detail_job, now_unix_secs());
        let mut detail_max = 0;
        terminal.draw(|f| {
            let (hits, max) = draw(f, app);
            list_hits = hits;
            detail_max = max;
        })?;
        app.detail_max_offset = detail_max;
        let animate = has_working_entry(app);
        let frame = if animate || update_check.is_some() || detail_job.is_some() {
            Duration::from_millis(125)
        } else {
            Duration::from_secs(1)
        };
        if !event::poll(frame)? {
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
            action @ (Action::Open | Action::OpenTemplate | Action::OpenTab(_)) => {
                // leave the TUI while the action runs: herdr CLI output goes to
                // the normal screen instead of corrupting the alternate screen
                cleanup_terminal(&mut terminal)?;
                let outcome = match action {
                    Action::OpenTab(number) => app.open_tab(number),
                    _ => app.open_selected(matches!(action, Action::OpenTemplate)),
                };
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

const WORKSPACE_REFRESH: Duration = Duration::from_secs(3);
const GIT_REFRESH: Duration = Duration::from_secs(30);

/// Reloads the snapshot and activity every few seconds. Git identities are
/// read again only once `GIT_REFRESH` has passed; until then they come from
/// the last load.
/// Keeps at most one git read in flight for the selected workspace, and stores
/// its result once it arrives.
fn drive_detail(app: &mut App, job: &mut Option<(String, Receiver<GitDetail>)>, now: i64) {
    match job.take() {
        Some((key, receiver)) => match receiver.try_recv() {
            Ok(detail) => app.cache_git(key, detail, now),
            Err(TryRecvError::Empty) => *job = Some((key, receiver)),
            Err(TryRecvError::Disconnected) => {}
        },
        None => {
            let Some((key, cwd)) = app.detail_target() else {
                return;
            };
            if app.git_refresh_due(&key, now) {
                *job = Some((key, spawn_git_read(cwd)));
            }
        }
    }
}

fn spawn_git_read(cwd: PathBuf) -> Receiver<GitDetail> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(detail::read_git(&cwd));
    });
    receiver
}

fn spawn_workspace_refresh(config: Config, mut known: OpenTopology) -> Receiver<Workspaces> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut git_read_at = Instant::now();
        loop {
            thread::sleep(WORKSPACE_REFRESH);
            let reuse_git = git_read_at.elapsed() < GIT_REFRESH;
            let workspaces = load_workspaces(&config, reuse_git.then_some(&known));
            if !reuse_git {
                git_read_at = Instant::now();
            }
            if !workspaces.topology.workspaces.is_empty() {
                known = workspaces.topology.clone();
            }
            if sender.send(workspaces).is_err() {
                break;
            }
        }
    });
    receiver
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

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Continue,
    Quit,
    Open,
    OpenTemplate,
    OpenTab(usize),
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
        .find_map(|binding| binding.command_for(app, key));

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
        Command::Open => app
            .selected_entry()
            .map(|_| Action::Open)
            .unwrap_or(Action::Continue),
        Command::OpenTemplate => app
            .selected_entry()
            .map(|_| Action::OpenTemplate)
            .unwrap_or(Action::Continue),
        Command::OpenLabel => {
            let slot = app
                .config
                .glance
                .labels()
                .iter()
                .position(|ch| key.code == KeyCode::Char(*ch));
            match slot.and_then(|slot| app.labelled_positions().get(slot).copied()) {
                Some(position) => {
                    app.selected = position;
                    Action::Open
                }
                None => Action::Continue,
            }
        }
        Command::OpenTab => match key.code {
            KeyCode::Char(ch) if ('1'..='9').contains(&ch) => {
                let number = usize::from(ch as u8 - b'0');
                if app.selected_tab_action(number).is_some() {
                    Action::OpenTab(number)
                } else {
                    Action::Continue
                }
            }
            _ => Action::Continue,
        },
        Command::OpenPrevious => match app.previous_position() {
            Some(position) => {
                app.selected = position;
                Action::Open
            }
            None => Action::Continue,
        },
        Command::NextGroup => {
            app.jump_group(true);
            Action::Continue
        }
        Command::PrevGroup => {
            app.jump_group(false);
            Action::Continue
        }
        Command::NextNeedsYou => {
            app.jump_needs_you();
            Action::Continue
        }
        Command::ScrollDetailDown => {
            app.scroll_detail(1);
            Action::Continue
        }
        Command::ScrollDetailUp => {
            app.scroll_detail(-1);
            Action::Continue
        }
        Command::Update => Action::Update,
        Command::MoveUp => {
            app.prev();
            Action::Continue
        }
        Command::MoveDown => {
            app.next();
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
            // The glance is the Workspace view, so its key clears the filter instead of toggling it.
            let filter = if source == Source::Workspace {
                None
            } else {
                Some(source)
            };
            app.set_filter(filter);
            app.apply_filter();
            Action::Continue
        }
    }
}

fn draw(f: &mut Frame, app: &App) -> (ListHits, usize) {
    let area = f.area();
    f.render_widget(Clear, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, app, rows[0]);
    let (list_hits, detail_max) = if app.glance_view() {
        draw_glance(f, app, rows[1])
    } else {
        (draw_list(f, app, rows[1]), 0)
    };

    draw_key_hints(f, app, rows[2]);
    if app.input_mode == InputMode::Help {
        draw_keybindings_help(f, app, area);
    }
    (list_hits, detail_max)
}

const HEADER_COUNTS_MIN_WIDTH: u16 = 90;

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .style(Style::default().bg(app.theme.panel_bg))
        .borders(Borders::BOTTOM);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    let counts = if area.width >= HEADER_COUNTS_MIN_WIDTH {
        group_count_spans(app)
    } else {
        Vec::new()
    };
    let counts_width: usize = counts.iter().map(Span::width).sum();
    let search_line = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(counts_width as u16)])
        .split(lines[0]);
    f.render_widget(
        Paragraph::new(Line::from(header_spans(app))),
        search_line[0],
    );
    f.render_widget(Paragraph::new(Line::from(counts)), search_line[1]);

    f.render_widget(
        Paragraph::new(Line::from(source_strip_spans(app))),
        lines[1],
    );
}

fn header_spans(app: &App) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(
            "Helm",
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    if app.input_mode == InputMode::Search || !app.query.is_empty() {
        spans.push(Span::styled("/ ", Style::default().fg(app.theme.subtext0)));
        spans.push(Span::styled(
            app.query.clone(),
            Style::default()
                .fg(app.theme.text)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::styled(
            "/ type to search",
            Style::default().fg(app.theme.subtext0),
        ));
    }
    if let Some(version) = &app.update_available {
        spans.push(Span::styled(
            format!("   ↑ v{version} available · F5 update"),
            Style::default()
                .fg(app.theme.yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

fn group_count_spans(app: &App) -> Vec<Span<'static>> {
    let counts = [
        (
            app.glance.values().filter(|row| row.needs_you).count(),
            "need you",
            Group::NeedsYou,
        ),
        (
            app.glance
                .values()
                .filter(|row| row.group == Group::Active)
                .count(),
            "active",
            Group::Active,
        ),
        (
            app.glance
                .values()
                .filter(|row| row.group == Group::Recent)
                .count(),
            "recent",
            Group::Recent,
        ),
        (
            app.glance
                .values()
                .filter(|row| row.group == Group::Quiet)
                .count(),
            "quiet",
            Group::Quiet,
        ),
    ];
    let mut spans = Vec::new();
    for (count, word, group) in counts {
        if count == 0 {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{count} {word}"),
            Style::default().fg(group_color(&app.theme, group)),
        ));
    }
    spans
}

fn source_strip_spans(app: &App) -> Vec<Span<'static>> {
    let bindings = keybindings(app);
    let mut spans = Vec::new();
    for binding in bindings
        .iter()
        .filter(|binding| matches!(binding.command(), Command::Filter(_)))
    {
        let (key_style, label_style) = if binding.is_active(app) {
            let chip = Style::default().fg(app.theme.panel_bg).bg(app.theme.accent);
            (chip.add_modifier(Modifier::BOLD), chip)
        } else {
            (
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(app.theme.subtext0),
            )
        };
        spans.push(Span::styled(
            format!(" {} ", binding.key_label(app)),
            key_style,
        ));
        spans.push(Span::styled(format!("{} ", binding.label), label_style));
        spans.push(Span::raw(" "));
    }
    spans
}

const HINT_GAP: &str = "   ";

fn hint_width(hints: &[(String, &'static str)]) -> usize {
    hints
        .iter()
        .map(|(key, label)| Span::raw(key.as_str()).width() + 1 + Span::raw(*label).width())
        .sum::<usize>()
        + HINT_GAP.len() * hints.len().saturating_sub(1)
}

fn hint_line(app: &App, hints: &[(String, &'static str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(HINT_GAP));
        }
        spans.push(Span::styled(
            key.clone(),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(app.theme.subtext0),
        ));
    }
    Line::from(spans)
}

/// One line of compact hints. The left block drops hints from its end until
/// it fits beside the right-aligned block.
fn draw_key_hints(f: &mut Frame, app: &App, area: Rect) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for binding in keybindings(app) {
        let Some(hint) = binding.compact_hint(app) else {
            continue;
        };
        if binding.footer_right {
            right.push(hint);
        } else {
            left.push(hint);
        }
    }

    let right_width = hint_width(&right);
    let room = (area.width as usize).saturating_sub(right_width + 2);
    while !left.is_empty() && hint_width(&left) > room {
        left.pop();
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_width as u16)])
        .split(area);
    let style = Style::default().bg(app.theme.panel_bg);
    f.render_widget(
        Paragraph::new(hint_line(app, &left)).style(style),
        columns[0],
    );
    f.render_widget(
        Paragraph::new(hint_line(app, &right)).style(style),
        columns[1],
    );
}

fn draw_keybindings_help(f: &mut Frame, app: &App, area: Rect) {
    let bindings = keybindings(app);
    let mut lines = Vec::new();
    for group in ["Move", "Open", "Find", "Sources", "Act"] {
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
    app.glance
        .values()
        .any(|row| row.state == AgentState::Working)
        || app.entries.iter().filter_map(entry_status).any(|status| {
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
    if let Some(status) = entry
        .search_terms
        .iter()
        .find_map(|term| term.strip_prefix("agent-status:"))
    {
        return Some(status);
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
    let source_budget = source_width.saturating_add(2);
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

/// Columns left of the signal: margin, label, icon, name, branch, progress
/// and mark, with a two-column gap after each of the six inner cells.
const GLANCE_FIXED_WIDTH: usize = 1 + 2 + 1 + 8 + 4 + 6 * 2;
const GLANCE_PROGRESS_WIDTH: usize = 8;
const GLANCE_MARK_WIDTH: usize = 4;
const GLANCE_SIGNAL_MAX: usize = 24;

struct GlanceCells {
    icon: &'static str,
    icon_color: Color,
    name: String,
    name_color: Color,
    branch: String,
    mark: Mark,
    signal: String,
}

fn glance_cells(app: &App, entry: &Entry) -> GlanceCells {
    match app.glance_row(entry) {
        Some(row) => GlanceCells {
            icon: glance::status_icon(row.state, app.config.glance.icons, app.spinner_tick),
            icon_color: agent_state_color(&app.theme, row.state),
            name: row.name.clone(),
            name_color: repository_color(&row.repository, &[], &app.theme),
            branch: row.branch.clone(),
            mark: row.mark,
            signal: glance::signal_text(&row.signal, app.now),
        },
        None => GlanceCells {
            icon: "",
            icon_color: app.theme.text,
            name: display_title(entry).to_string(),
            name_color: app.theme.text,
            branch: display_path(entry),
            mark: Mark::None,
            signal: String::new(),
        },
    }
}

fn agent_state_color(theme: &Theme, state: AgentState) -> Color {
    match state {
        AgentState::Blocked => theme.red,
        AgentState::Done => theme.teal,
        AgentState::Working => theme.yellow,
        AgentState::Idle => theme.green,
        AgentState::Unknown => theme.overlay0,
    }
}

fn group_color(theme: &Theme, group: Group) -> Color {
    match group {
        Group::Pinned => theme.accent,
        Group::NeedsYou => theme.red,
        Group::Active => theme.yellow,
        Group::Recent => theme.green,
        Group::Quiet => theme.overlay0,
        Group::Directories | Group::Other => theme.subtext0,
    }
}

fn fit_cell(value: &str, width: usize) -> String {
    let clipped = truncate_terminal(value, width);
    let padding = width.saturating_sub(Span::raw(&clipped).width());
    format!("{clipped}{}", " ".repeat(padding))
}

const DETAIL_MIN_WIDTH: u16 = 110;
const DETAIL_SECTION_MIN_ROWS: usize = 3;
const DETAIL_SECTION_MAX_ROWS: usize = 6;
const DETAIL_NUMBER_WIDTH: usize = 6;
const SUBAGENT_BAR_CELLS: usize = 10;

/// The glance list, with the detail pane on the right when the list is wide enough.
/// Returns the list hits and the largest useful detail scroll offset.
fn draw_glance(f: &mut Frame, app: &App, area: Rect) -> (ListHits, usize) {
    if area.width < DETAIL_MIN_WIDTH {
        return (draw_glance_list(f, app, area), 0);
    }
    let [list, border, detail] = Layout::horizontal([
        Constraint::Percentage(55),
        Constraint::Length(1),
        Constraint::Percentage(45),
    ])
    .areas(area);
    f.render_widget(
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(app.theme.surface1)),
        border,
    );
    let max_offset = draw_detail(f, app, detail);
    (draw_glance_list(f, app, list), max_offset)
}

fn draw_glance_list(f: &mut Frame, app: &App, area: Rect) -> ListHits {
    let visible: Vec<(usize, Group, GlanceCells)> = app
        .filtered
        .iter()
        .enumerate()
        .filter_map(|(position, index)| {
            let entry = &app.entries[*index];
            let group = app.glance_group(entry)?;
            Some((position, group, glance_cells(app, entry)))
        })
        .collect();

    let name_width = visible
        .iter()
        .map(|(_, _, cells)| Span::raw(&cells.name).width())
        .max()
        .unwrap_or(0);
    let branch_width = visible
        .iter()
        .map(|(_, _, cells)| Span::raw(&cells.branch).width())
        .max()
        .unwrap_or(0);
    let signal_width = visible
        .iter()
        .map(|(_, _, cells)| Span::raw(&cells.signal).width())
        .max()
        .unwrap_or(0)
        .min(GLANCE_SIGNAL_MAX);
    let flex = (area.width as usize).saturating_sub(GLANCE_FIXED_WIDTH + signal_width);
    let name_width = name_width.min(flex * 3 / 5);
    let branch_width = branch_width.min(flex.saturating_sub(name_width));

    let labels = app.config.glance.labels();
    let labelled = if app.input_mode == InputMode::Normal {
        app.labelled_positions()
    } else {
        Vec::new()
    };
    let mut items = Vec::new();
    let mut hit_rows = Vec::new();
    let mut selected_item = None;
    let mut current: Option<Group> = None;
    for (position, group, cells) in &visible {
        if current != Some(*group) {
            if current.is_some() {
                items.push(ListItem::new(Line::raw("")));
                hit_rows.push(None);
            }
            let count = visible.iter().filter(|(_, g, _)| g == group).count();
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {}", group.label().to_uppercase()),
                    Style::default()
                        .fg(group_color(&app.theme, *group))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" · {count}"),
                    Style::default().fg(app.theme.subtext0),
                ),
            ])));
            hit_rows.push(None);
            current = Some(*group);
        }
        if *position == app.selected {
            selected_item = Some(items.len());
        }
        let mark_color = match cells.mark {
            Mark::Here => app.theme.accent,
            Mark::Last | Mark::None => app.theme.subtext0,
        };
        let mark = match cells.mark {
            Mark::Here => "here",
            Mark::Last => "last",
            Mark::None => "",
        };
        let label = match labelled
            .iter()
            .position(|&slot| slot == *position)
            .and_then(|slot| labels.get(slot))
        {
            Some(ch) => Span::styled(
                format!(" {ch} "),
                Style::default()
                    .fg(app.theme.accent)
                    .bg(app.theme.surface0)
                    .add_modifier(Modifier::BOLD),
            ),
            None => Span::raw("   "),
        };
        let mut spans = vec![
            Span::raw(" "),
            label,
            Span::raw(" "),
            Span::styled(
                fit_cell(cells.icon, 1),
                Style::default().fg(cells.icon_color),
            ),
            Span::raw("  "),
            Span::styled(
                fit_cell(&cells.name, name_width),
                Style::default()
                    .fg(cells.name_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                fit_cell(&cells.branch, branch_width),
                Style::default().fg(app.theme.subtext0),
            ),
            Span::raw("  "),
            Span::styled(
                fit_cell(mark, GLANCE_MARK_WIDTH),
                Style::default().fg(mark_color),
            ),
            Span::raw("  "),
            Span::styled(
                fit_cell(&cells.signal, signal_width),
                Style::default().fg(app.theme.subtext0),
            ),
        ];
        if let Some(bar) = progress_cell(app, *position) {
            spans.push(Span::raw("  "));
            spans.push(bar);
        }
        items.push(ListItem::new(Line::from(spans)));
        hit_rows.push(Some(*position));
    }

    if items.is_empty() {
        let message = if app.query.trim().is_empty() {
            "no open workspaces"
        } else {
            "no destination matches that query"
        };
        items.push(ListItem::new(Line::styled(
            message,
            Style::default().fg(app.theme.overlay0),
        )));
        hit_rows.push(None);
    }

    let mut state = ListState::default();
    state.select(selected_item);
    let list =
        List::new(items).highlight_style(Style::default().bg(app.theme.selection_background(true)));
    f.render_stateful_widget(list, area, &mut state);

    let mut rows = Vec::new();
    for (line, hit) in hit_rows.into_iter().enumerate().skip(state.offset()) {
        let y = area.y + (line - state.offset()) as u16;
        if y >= area.bottom() {
            break;
        }
        if let Some(position) = hit {
            rows.push((y..y + 1, position));
        }
    }
    ListHits { area, rows }
}

/// The summed subagent bar of a workspace with a working agent, `None` otherwise.
fn progress_cell(app: &App, position: usize) -> Option<Span<'static>> {
    let workspace = app
        .filtered
        .get(position)
        .and_then(|index| app.workspace_node(&app.entries[*index]))?;
    let working = workspace
        .tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .any(|pane| {
            pane.agent
                .as_ref()
                .is_some_and(|agent| agent.state == AgentState::Working)
        });
    if !working {
        return None;
    }
    let (done, total) = app.subagent_progress(workspace)?;
    Some(Span::styled(
        bar(done, total, GLANCE_PROGRESS_WIDTH),
        Style::default().fg(app.theme.yellow),
    ))
}

/// `█` for the done share and `░` for the rest, rounded to the nearest cell.
fn bar(done: u32, total: u32, cells: usize) -> String {
    let filled = if total == 0 {
        0
    } else {
        let done = done.min(total) as usize;
        (done * cells + total as usize / 2) / total as usize
    };
    format!("{}{}", "█".repeat(filled), "░".repeat(cells - filled))
}

/// Draws the detail pane and returns the largest useful scroll offset. The header
/// and agents sit at the top, and the changes and commits sections are anchored
/// to the bottom with a height that depends only on the pane height.
fn draw_detail(f: &mut Frame, app: &App, area: Rect) -> usize {
    let Some(SelectedWorkspace {
        entry,
        workspace,
        row,
        key,
    }) = app.selected_workspace()
    else {
        return 0;
    };
    let area = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(3),
        ..area
    };
    let width = area.width as usize;
    let dim = Style::default().fg(app.theme.subtext0);

    let chips = tab_chips(app, workspace);
    let chips_width: usize = chips.iter().map(|span| span.width()).sum();
    let name_room = width.saturating_sub(chips_width + 1);
    let name = truncate_terminal(&row.name, name_room);
    let branch_room = name_room.saturating_sub(Span::raw(&name).width() + 2);
    let mut left = vec![Span::styled(
        name.clone(),
        Style::default()
            .fg(app.theme.text)
            .add_modifier(Modifier::BOLD),
    )];
    if branch_room > 0 && !row.branch.is_empty() {
        left.push(Span::styled(
            format!("  {}", truncate_terminal(&row.branch, branch_room)),
            dim,
        ));
    }
    let header = vec![
        justified(left, chips, width),
        Line::styled(truncate_end(&display_path(entry), width), dim),
        Line::styled(truncate_end(&activity_sources(app, workspace), width), dim),
        Line::styled("─".repeat(width), Style::default().fg(app.theme.surface1)),
        Line::raw(""),
    ];

    let git = app.git_detail.get(&key).map(|cached| &cached.detail);
    let changes: &[Change] = git.map_or(&[], |git| &git.changes);
    let commits: &[Commit] = git.map_or(&[], |git| &git.commits);
    let rows = ((area.height as usize).saturating_sub(header.len() + 3) / 2)
        .clamp(DETAIL_SECTION_MIN_ROWS, DETAIL_SECTION_MAX_ROWS);
    let lower_height = (3 + 2 * rows).min(area.height as usize) as u16;
    let [top, bottom] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(lower_height)]).areas(area);

    let mut top_lines = header;
    top_lines.extend(agent_lines(app, workspace, width));
    f.render_widget(Paragraph::new(top_lines), top);

    let changes_overflow = changes.len().saturating_sub(rows);
    let commits_overflow = commits.len().saturating_sub(rows);
    let max_offset = changes_overflow + commits_overflow;
    let offset = app.detail_scroll.offset.min(max_offset);
    let changes_start = offset.min(changes_overflow);
    let commits_start = (offset - changes_start).min(commits_overflow);
    let loaded = git.is_some();
    let mut lower = changes_section(app, changes, loaded, changes_start, rows, width);
    lower.push(Line::raw(""));
    lower.extend(commits_section(
        app,
        commits,
        loaded,
        commits_start,
        rows,
        width,
    ));
    f.render_widget(Paragraph::new(lower), bottom);
    max_offset
}

/// The activity behind the row's age: the last commit and the last focus.
fn activity_sources(app: &App, workspace: &WorkspaceNode) -> String {
    let commit = workspace
        .git
        .as_ref()
        .and_then(|git| git.last_commit_at)
        .map(|at| format!("last commit {}", glance::age(app.now, at)));
    let focus = app
        .activity
        .workspaces
        .get(&workspace.id)
        .map(|recorded| format!("focused {}", glance::age(app.now, recorded.focused_at)));
    [commit, focus]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn tab_chips(app: &App, workspace: &WorkspaceNode) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, tab) in workspace.tabs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" "));
        }
        let number = format!(" {} ", index + 1);
        let label = format!(" {} ", tab.label);
        if tab.focused {
            let style = Style::default()
                .fg(app.theme.base_background)
                .bg(app.theme.accent);
            spans.push(Span::styled(number, style));
            spans.push(Span::styled(label, style));
        } else {
            spans.push(Span::styled(
                number,
                Style::default().fg(app.theme.accent).bg(app.theme.surface0),
            ));
            spans.push(Span::styled(label, Style::default().fg(app.theme.subtext0)));
        }
    }
    spans
}

fn agent_lines(app: &App, workspace: &WorkspaceNode, width: usize) -> Vec<Line<'static>> {
    let panes: Vec<&PaneNode> = workspace
        .tabs
        .iter()
        .flat_map(|tab| &tab.panes)
        .filter(|pane| pane.agent.is_some())
        .collect();
    if panes.is_empty() {
        return Vec::new();
    }
    let dim = Style::default().fg(app.theme.subtext0);
    let mut lines = vec![Line::styled("AGENTS", dim)];
    for pane in panes {
        let Some(agent) = pane.agent.as_ref() else {
            continue;
        };
        let since = app
            .activity
            .panes
            .get(&pane.id)
            .filter(|recorded| AgentState::parse(&recorded.status) == agent.state)
            .map(|recorded| recorded.since);
        let elapsed = since
            .map(|since| glance::age(app.now, since))
            .unwrap_or_default();
        let title = match &pane.title {
            Some(title) => format!("{} · {title}", agent.name),
            None => agent.name.clone(),
        };
        let title = truncate_terminal(&title, width.saturating_sub(elapsed.len() + 4));
        let icon = glance::status_icon(agent.state, app.config.glance.icons, app.spinner_tick);
        lines.push(justified(
            vec![
                Span::styled(
                    icon,
                    Style::default().fg(agent_state_color(&app.theme, agent.state)),
                ),
                Span::raw(" "),
                Span::styled(title, Style::default().fg(app.theme.text)),
            ],
            vec![Span::styled(elapsed, dim)],
            width,
        ));
        for subagent in app.subagents.get(&pane.id).into_iter().flatten() {
            lines.push(subagent_line(app, subagent, width));
        }
    }
    lines
}

fn subagent_line(app: &App, subagent: &Subagent, width: usize) -> Line<'static> {
    let dim = Style::default().fg(app.theme.subtext0);
    let ratio = format!("{}/{}", subagent.done.min(subagent.total), subagent.total);
    let elapsed = glance::age(app.now, subagent.started_at);
    let right_width = SUBAGENT_BAR_CELLS + 1 + ratio.len() + 2 + elapsed.len();
    let step = truncate_terminal(&subagent.step, width.saturating_sub(4 + right_width + 1));
    justified(
        vec![
            Span::styled("  └ ", dim),
            Span::styled(step, Style::default().fg(app.theme.text)),
        ],
        vec![
            Span::styled(
                bar(subagent.done, subagent.total, SUBAGENT_BAR_CELLS),
                Style::default().fg(app.theme.yellow),
            ),
            Span::styled(format!(" {ratio}  {elapsed}"), dim),
        ],
        width,
    )
}

fn changes_section(
    app: &App,
    changes: &[Change],
    loaded: bool,
    start: usize,
    rows: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(app.theme.subtext0);
    let added: u32 = changes.iter().filter_map(|change| change.added).sum();
    let removed: u32 = changes.iter().filter_map(|change| change.removed).sum();
    let (heading, totals) = if !loaded || changes.is_empty() {
        ("CHANGES".to_string(), Vec::new())
    } else {
        let heading = match changes.len() {
            1 => "CHANGES · 1 file".to_string(),
            count => format!("CHANGES · {count} files"),
        };
        (heading, count_spans(Some(added), Some(removed), app))
    };
    let mut body = Vec::new();
    if !loaded {
        body.push(Line::styled("…", dim));
    } else if changes.is_empty() {
        body.push(Line::styled("clean", dim));
    } else {
        let (visible, hidden) = window(changes, start, rows);
        let path_room = width.saturating_sub(2 * DETAIL_NUMBER_WIDTH + 2);
        for change in visible {
            let state_color = match change.state {
                'A' => app.theme.green,
                'D' => app.theme.red,
                _ => app.theme.yellow,
            };
            body.push(justified(
                vec![
                    Span::styled(change.state.to_string(), Style::default().fg(state_color)),
                    Span::raw(" "),
                    Span::raw(truncate_left(&change.path, path_room)),
                ],
                count_spans(change.added, change.removed, app),
                width,
            ));
        }
        if let Some(hidden) = hidden {
            body.push(Line::styled(format!("↓ {hidden} more"), dim));
        }
    }
    let mut lines = vec![justified(vec![Span::styled(heading, dim)], totals, width)];
    lines.extend(pad_rows(body, rows));
    lines
}

fn commits_section(
    app: &App,
    commits: &[Commit],
    loaded: bool,
    start: usize,
    rows: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let dim = Style::default().fg(app.theme.subtext0);
    let mut body = Vec::new();
    if !loaded {
        body.push(Line::styled("…", dim));
    } else if commits.is_empty() {
        body.push(Line::styled("no commits", dim));
    } else {
        let (visible, hidden) = window(commits, start, rows);
        for commit in visible {
            let age = glance::age(app.now, commit.committed_at);
            let subject_room = width.saturating_sub(commit.hash.len() + 2 + age.len() + 1);
            body.push(justified(
                vec![
                    Span::styled(commit.hash.clone(), Style::default().fg(app.theme.accent)),
                    Span::raw("  "),
                    Span::raw(truncate_end(&commit.subject, subject_room)),
                ],
                vec![Span::styled(age, dim)],
                width,
            ));
        }
        if let Some(hidden) = hidden {
            body.push(Line::styled(format!("↓ {hidden} more"), dim));
        }
    }
    let mut lines = vec![Line::styled("RECENT COMMITS", dim)];
    lines.extend(pad_rows(body, rows));
    lines
}

/// The rows of `items` from `start`. When more follow, the last row becomes a
/// count of the hidden items.
fn window<T>(items: &[T], start: usize, rows: usize) -> (&[T], Option<usize>) {
    let rest = items.len().saturating_sub(start);
    if rest <= rows {
        (&items[start..], None)
    } else {
        (&items[start..start + rows - 1], Some(rest - (rows - 1)))
    }
}

fn pad_rows(mut body: Vec<Line<'static>>, rows: usize) -> Vec<Line<'static>> {
    body.resize_with(rows, || Line::raw(""));
    body
}

fn count_spans(added: Option<u32>, removed: Option<u32>, app: &App) -> Vec<Span<'static>> {
    let shown = |value: Option<u32>, sign: char| {
        value
            .filter(|count| *count > 0)
            .map(|count| format!("{sign}{count}"))
            .unwrap_or_default()
    };
    vec![
        Span::styled(
            format!("{:>width$}", shown(added, '+'), width = DETAIL_NUMBER_WIDTH),
            Style::default().fg(app.theme.green),
        ),
        Span::styled(
            format!(
                "{:>width$}",
                shown(removed, '-'),
                width = DETAIL_NUMBER_WIDTH
            ),
            Style::default().fg(app.theme.red),
        ),
    ]
}

/// Puts `left` at the start and `right` at the end of one `width`-wide line.
fn justified(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(|span| span.width()).sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    spans.extend(right);
    Line::from(spans)
}

fn truncate_left(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.into();
    }
    if max_chars == 0 {
        return String::new();
    }
    std::iter::once('…')
        .chain(value.chars().skip(count - (max_chars - 1)))
        .collect()
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) -> ListHits {
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
                        status_icon_at(
                            e.source == Source::Workspace && e.agent_target.is_none(),
                            status,
                            app.spinner_tick
                        )
                    )
                })
                .unwrap_or_default();
            let raw_path = e.path.display().to_string();
            let raw_metadata = entry_metadata(e);
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
                .saturating_sub(source_width.saturating_add(2))
                .saturating_sub(detailed_layout.status_width)
                .saturating_sub(detailed_layout.title_width)
                .saturating_sub(2)
                .saturating_sub(detailed_layout.right_width)
                .saturating_sub(usize::from(detailed_layout.right_width > 0));
            let title = truncate_end(display_title(e), detailed_layout.title_width);
            let path = truncate_end(&display_path(e), path_width);
            let status_color = status
                .map(|status| agent_status_color(&app.theme, status))
                .unwrap_or(color);
            let mut title_spans = vec![
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
            ];
            if detailed_layout.right_width > 0 {
                title_spans.push(Span::raw(" "));
                if !metadata.is_empty() {
                    title_spans.push(Span::styled(
                        metadata.clone(),
                        Style::default().fg(app.theme.overlay0),
                    ));
                }
                title_spans.push(Span::raw(
                    " ".repeat(detailed_layout.right_width.saturating_sub(metadata_width)),
                ));
            }
            title_spans.push(Span::raw(" "));
            title_spans.extend(source_spans(app, e, source_width));
            title_spans.push(Span::raw(" "));
            title_spans.push(Span::styled(status_text, Style::default().fg(status_color)));
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
        .borders(Borders::RIGHT)
        .style(Style::default().bg(app.theme.base_background));
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
    }
}

fn source_color(theme: &Theme, source: &Source) -> Color {
    match source {
        Source::Workspace => theme.green,
        Source::Zoxide => theme.blue,
        Source::Root => theme.teal,
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
        activity::ActivityState,
        app::{pin_key, Workspaces},
        config::Config,
        model::EntryAction,
        theme::Theme,
        topology::{
            AgentMeta, AgentState, GitHead, GitIdentity, OpenTopology, PaneNode, TabNode,
            WorkspaceNode,
        },
    };
    use crossterm::event::{MouseButton, MouseEventKind};

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

    fn entry(source: Source, title: &str) -> Entry {
        Entry {
            source,
            title: title.into(),
            subtitle: String::new(),
            path: PathBuf::from(title),
            workspace_id: None,
            workspace_label: None,
            agent_target: None,
            action: EntryAction::FocusOrCreateDir,
            source_label: None,
            search_terms: vec![],
        }
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
    fn alt_enter_opens_selected_directory_with_configured_template() {
        let mut config = Config::default();
        config.picker.directory_template = Some("default.toml".into());
        let mut app = App::new(config, Theme::terminal());
        app.entries = vec![entry(Source::Zoxide, "/tmp")];
        app.source_filter = Some(Source::Zoxide);
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
        assert!(normal_text.contains("/ type to search"));

        handle_key(&mut app, key(KeyCode::Char('/')));
        handle_key(&mut app, key(KeyCode::Char('x')));
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let search_text = buffer_text(&terminal);
        assert!(search_text.contains("/ x"));
        assert!(!search_text.contains("type to search"));
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
    fn picker_default_surfaces_use_the_base_background() {
        let app = App::new(Config::default(), Theme::terminal());
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(0, 0)].bg, app.theme.base_background);
        assert_eq!(buffer[(79, 1)].bg, app.theme.base_background);
        assert_eq!(buffer[(0, 6)].bg, app.theme.base_background);
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
        assert!(text.contains("zoxide"));
        assert!(!text.contains("?/?"));

        handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn registry_maps_ctrl_b_to_mark_without_stealing_enter() {
        let app = App::new(Config::default(), Theme::terminal());
        let bindings = keybindings(&app);
        let mark = bindings
            .iter()
            .find(|binding| binding.command() == Command::ToggleMark)
            .unwrap();

        assert_eq!(mark.label, "pin to the top");
        assert_eq!(
            mark.command_for(
                &app,
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)
            ),
            Some(Command::ToggleMark)
        );
        assert_eq!(
            mark.command_for(&app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
        let open = bindings
            .iter()
            .find(|binding| binding.command() == Command::Open)
            .unwrap();
        assert_eq!(
            open.command_for(&app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Command::Open)
        );
    }

    #[test]
    fn workspace_key_returns_to_the_glance_from_another_source() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.source_filter = Some(Source::Root);
        let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        handle_key(&mut app, ctrl_w);
        assert_eq!(app.source_filter, None);
        handle_key(&mut app, ctrl_w);
        assert_eq!(app.source_filter, None);
    }

    #[test]
    fn disabled_sources_are_absent_from_filter_bindings_and_footer() {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.config.sources.zoxide = false;

        let bindings = keybindings(&app);
        assert!(!bindings
            .iter()
            .any(|binding| { matches!(binding.command(), Command::Filter(Source::Zoxide)) }));

        let backend = TestBackend::new(110, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(!text.contains("zoxide"));
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

    fn pane(id: &str, cwd: &str, agent: Option<(&str, AgentState)>) -> PaneNode {
        PaneNode {
            id: id.into(),
            label: id.into(),
            cwd: PathBuf::from(cwd),
            focused: false,
            title: None,
            agent_session: None,
            agent: agent.map(|(name, state)| AgentMeta {
                name: name.into(),
                state,
            }),
        }
    }

    fn glance_workspace(
        id: &str,
        label: &str,
        focused: bool,
        last_commit_at: Option<i64>,
        panes: Vec<PaneNode>,
    ) -> WorkspaceNode {
        WorkspaceNode {
            id: id.into(),
            label: label.into(),
            session: None,
            focused,
            tabs: vec![TabNode {
                id: format!("{id}:t1"),
                label: "Code".into(),
                focused: true,
                panes,
            }],
            git: Some(GitIdentity {
                repo_key: format!("/repos/{label}/.git"),
                label: label.into(),
                head: GitHead::Branch(format!("{label}-branch")),
                last_commit_at,
            }),
        }
    }

    fn glance_app(
        now: i64,
        previous: Option<&str>,
        workspaces: Vec<WorkspaceNode>,
        activity: ActivityState,
    ) -> App {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology { workspaces },
                activity,
                previous: previous.map(str::to_string),
                subagents: Default::default(),
            },
            now,
        );
        app
    }

    fn render_app(app: &App, width: u16, height: u16) -> (Vec<String>, ListHits) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut hits = ListHits::default();
        terminal.draw(|f| hits = draw(f, app).0).unwrap();
        let text = buffer_text(&terminal);
        (text.lines().map(str::to_string).collect(), hits)
    }

    fn line_index(lines: &[String], needle: &str) -> usize {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line contains {needle:?}"))
    }

    fn column_of(line: &str, needle: &str) -> usize {
        let byte = line.find(needle).unwrap();
        line[..byte].chars().count()
    }

    const NOW: i64 = 1_000_000;

    #[test]
    fn glance_groups_render_in_order_with_a_blank_line_between_them() {
        let mut activity = ActivityState::default();
        activity.record_agent_status("p-helm", "working", NOW - 14 * 60);
        activity.record_agent_status("p-skills", "blocked", NOW - 4 * 60);
        let app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace(
                    "w1",
                    "helm",
                    true,
                    None,
                    vec![pane(
                        "p-helm",
                        "/repos/helm",
                        Some(("claude", AgentState::Working)),
                    )],
                ),
                glance_workspace(
                    "w2",
                    "skills",
                    false,
                    None,
                    vec![pane(
                        "p-skills",
                        "/repos/skills",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
                glance_workspace("w3", "koyo", false, Some(NOW - 3 * 86_400), vec![]),
                glance_workspace("w4", "lazyjira", false, Some(NOW - 2 * 3_600), vec![]),
            ],
            activity,
        );

        let (lines, _) = render_app(&app, 100, 20);
        let headings = ["NEEDS YOU · 1", "ACTIVE · 1", "RECENT · 1", "QUIET · 1"]
            .map(|label| line_index(&lines, label));
        assert!(headings.windows(2).all(|pair| pair[0] < pair[1]));
        for heading in &headings[1..] {
            assert!(lines[heading - 1].trim().is_empty());
        }
        assert!(lines[headings[0] + 1].contains("skills"));
        assert!(lines[headings[1] + 1].contains("helm"));
        assert!(lines[headings[2] + 1].contains("lazyjira"));
        assert!(lines[headings[3] + 1].contains("koyo"));
        assert!(!lines.iter().any(|line| line.contains('→')));
    }

    #[test]
    fn glance_rows_align_branch_and_signal_columns_across_rows() {
        let mut activity = ActivityState::default();
        activity.record_agent_status("p-helm", "working", NOW - 14 * 60);
        activity.record_agent_status("p-skills", "blocked", NOW - 4 * 60);
        let app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace(
                    "w1",
                    "helm",
                    true,
                    None,
                    vec![pane(
                        "p-helm",
                        "/repos/helm",
                        Some(("claude", AgentState::Working)),
                    )],
                ),
                glance_workspace(
                    "w2",
                    "skills",
                    false,
                    None,
                    vec![pane(
                        "p-skills",
                        "/repos/skills",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
                glance_workspace("w3", "lazyjira", false, Some(NOW - 2 * 3_600), vec![]),
            ],
            activity,
        );

        let (lines, _) = render_app(&app, 100, 20);
        let rows = [
            ("helm-branch", "helm", "14m"),
            ("skills-branch", "skills", "4m"),
            ("lazyjira-branch", "lazyjira", "2h"),
        ];
        let branch_columns: Vec<usize> = rows
            .iter()
            .map(|(branch, _, _)| column_of(&lines[line_index(&lines, branch)], branch))
            .collect();
        assert!(branch_columns.windows(2).all(|pair| pair[0] == pair[1]));
        let signal_starts: Vec<usize> = rows
            .iter()
            .map(|(_, name, signal)| {
                let line = &lines[line_index(&lines, name)];
                column_of(line, signal)
            })
            .collect();
        assert!(signal_starts.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn here_and_last_marks_the_focused_and_previous_workspace() {
        let app = glance_app(
            NOW,
            Some("w2"),
            vec![
                glance_workspace("w1", "helm", true, Some(NOW - 60), vec![]),
                glance_workspace("w2", "skills", false, Some(NOW - 120), vec![]),
                glance_workspace("w3", "koyo", false, Some(NOW - 180), vec![]),
            ],
            ActivityState::default(),
        );

        let (lines, _) = render_app(&app, 100, 20);
        assert!(lines[line_index(&lines, "helm")].contains("here"));
        assert!(lines[line_index(&lines, "skills")].contains("last"));
        assert!(!lines[line_index(&lines, "koyo")].contains("here"));
        assert!(!lines[line_index(&lines, "koyo")].contains("last"));
    }

    #[test]
    fn directories_group_appears_only_while_a_query_is_active() {
        let mut app = glance_app(
            NOW,
            None,
            vec![glance_workspace("w1", "helm", true, Some(NOW - 60), vec![])],
            ActivityState::default(),
        );
        app.entries.push(entry(Source::Zoxide, "projects-dir"));
        app.apply_filter();
        let (lines, _) = render_app(&app, 110, 20);
        assert!(!lines.iter().any(|line| line.contains("DIRECTORIES")));

        app.query = "projects".into();
        app.apply_filter();
        let (lines, _) = render_app(&app, 110, 20);
        let heading = line_index(&lines, "DIRECTORIES · 1");
        assert!(lines[heading + 1].contains("projects-dir"));
        assert!(!lines.iter().any(|line| line.contains("helm")));
    }

    #[test]
    fn search_keeps_the_group_layout_and_drops_non_matching_rows() {
        let mut activity = ActivityState::default();
        activity.record_agent_status("p-skills", "blocked", NOW - 4 * 60);
        let mut app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace("w1", "helm", true, Some(NOW - 60), vec![]),
                glance_workspace(
                    "w2",
                    "skills",
                    false,
                    None,
                    vec![pane(
                        "p-skills",
                        "/repos/skills",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
            ],
            activity,
        );
        app.query = "skills".into();
        app.apply_filter();

        let (lines, _) = render_app(&app, 110, 20);
        assert!(lines[line_index(&lines, "NEEDS YOU · 1")].contains("NEEDS YOU"));
        assert!(!lines.iter().any(|line| line.contains("helm")));
    }

    #[test]
    fn clicking_a_glance_row_selects_that_workspace() {
        let mut app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace("w1", "helm", true, Some(NOW - 60), vec![]),
                glance_workspace("w2", "lazyjira", false, Some(NOW - 2 * 3_600), vec![]),
            ],
            ActivityState::default(),
        );
        let (lines, hits) = render_app(&app, 110, 20);
        let row = line_index(&lines, "lazyjira") as u16;
        let (_, position) = hits
            .rows
            .iter()
            .find(|(range, _)| range.contains(&row))
            .unwrap();
        assert_eq!(*position, 1);

        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row,
                modifiers: KeyModifiers::NONE,
            },
            &hits,
        );
        assert_eq!(app.selected_entry().unwrap().title, "lazyjira");
    }

    #[test]
    fn working_agent_keeps_the_spinner_loop_active() {
        let mut activity = ActivityState::default();
        activity.record_agent_status("p1", "working", NOW - 60);
        let working = glance_app(
            NOW,
            None,
            vec![glance_workspace(
                "w1",
                "helm",
                true,
                None,
                vec![pane(
                    "p1",
                    "/repos/helm",
                    Some(("claude", AgentState::Working)),
                )],
            )],
            activity,
        );
        assert!(has_working_entry(&working));

        let idle = glance_app(
            NOW,
            None,
            vec![glance_workspace("w1", "helm", true, Some(NOW - 60), vec![])],
            ActivityState::default(),
        );
        assert!(!has_working_entry(&idle));
    }

    #[test]
    fn glance_renders_a_realistic_list_at_160_by_40() {
        let mut activity = ActivityState::default();
        activity.record_focus("w1", NOW - 6 * 60);
        activity.record_agent_status("p-helm", "working", NOW - 14 * 60);
        activity.record_agent_status("p-skills", "blocked", NOW - 4 * 60);
        activity.record_agent_status("p-writing", "done", NOW - 12 * 60);
        activity.record_agent_status("p-mdn", "working", NOW - 14 * 60);
        activity.record_focus("w5", NOW - 38 * 60);
        let mut app = glance_app(
            NOW,
            Some("w2"),
            vec![
                glance_workspace(
                    "w1",
                    "helm.herdr",
                    true,
                    Some(NOW - 6 * 60),
                    vec![pane(
                        "p-helm",
                        "/repos/helm",
                        Some(("claude", AgentState::Working)),
                    )],
                ),
                glance_workspace("w2", "dots", false, Some(NOW - 3 * 3_600), vec![]),
                glance_workspace(
                    "w3",
                    "skills",
                    false,
                    None,
                    vec![pane(
                        "p-skills",
                        "/repos/skills",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
                glance_workspace(
                    "w4",
                    "writing",
                    false,
                    None,
                    vec![pane(
                        "p-writing",
                        "/repos/writing",
                        Some(("claude", AgentState::Done)),
                    )],
                ),
                glance_workspace(
                    "w5",
                    "mdn.nvim",
                    false,
                    None,
                    vec![pane(
                        "p-mdn",
                        "/repos/mdn",
                        Some(("claude", AgentState::Working)),
                    )],
                ),
                glance_workspace("w6", "lazyjira", false, Some(NOW - 2 * 3_600), vec![]),
                glance_workspace("w7", "koyo", false, Some(NOW - 3 * 86_400), vec![]),
                glance_workspace("w8", "fonts", false, Some(NOW - 9 * 86_400), vec![]),
            ],
            activity,
        );
        let dots = app
            .topology_entries
            .iter()
            .find(|entry| entry.title == "dots")
            .unwrap()
            .clone();
        app.pinned_entries.insert(pin_key(&dots));
        app.rebuild_glance();
        app.apply_filter();

        let (lines, _) = render_app(&app, 160, 40);
        assert!(line_index(&lines, "PINNED") < line_index(&lines, "NEEDS YOU"));
        assert!(lines[line_index(&lines, "fonts")].contains("9d"));
    }

    fn shift_key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT)
    }

    fn selected_title(app: &App) -> String {
        app.selected_entry().unwrap().title.clone()
    }

    fn commit_workspace(id: &str, label: &str, last_commit_at: i64) -> WorkspaceNode {
        glance_workspace(id, label, false, Some(last_commit_at), vec![])
    }

    #[test]
    fn label_opens_the_labelled_row_and_is_ignored_while_searching() {
        let mut app = glance_app(
            NOW,
            None,
            vec![
                commit_workspace("w1", "alpha", NOW - 60),
                commit_workspace("w2", "beta", NOW - 120),
                commit_workspace("w3", "gamma", NOW - 180),
            ],
            ActivityState::default(),
        );
        assert_eq!(selected_title(&app), "alpha");

        assert_eq!(handle_key(&mut app, key(KeyCode::Char('s'))), Action::Open);
        assert_eq!(selected_title(&app), "beta");

        handle_key(&mut app, key(KeyCode::Char('/')));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('s'))),
            Action::Continue
        );
        assert_eq!(app.query, "s");
    }

    #[test]
    fn digits_open_the_tab_of_the_selected_workspace_and_ignore_missing_tabs() {
        let mut workspace = glance_workspace("w1", "helm", true, Some(NOW - 60), vec![]);
        workspace.tabs.push(TabNode {
            id: "w1:t2".into(),
            label: "Edit".into(),
            focused: false,
            panes: vec![],
        });
        let mut app = glance_app(NOW, None, vec![workspace], ActivityState::default());

        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('2'))),
            Action::OpenTab(2)
        );
        assert!(matches!(
            app.selected_tab_action(2),
            Some(EntryAction::FocusTab { id, .. }) if id == "w1:t2"
        ));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('3'))),
            Action::Continue
        );
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('9'))),
            Action::Continue
        );
    }

    #[test]
    fn group_jumps_land_on_group_starts_and_stop_at_the_edges() {
        let mut activity = ActivityState::default();
        activity.record_agent_status("p-need", "blocked", NOW - 60);
        let mut app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace(
                    "w1",
                    "need",
                    false,
                    None,
                    vec![pane(
                        "p-need",
                        "/repos/need",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
                commit_workspace("w2", "active-one", NOW - 60),
                commit_workspace("w3", "active-two", NOW - 120),
                commit_workspace("w4", "old", NOW - 9 * 86_400),
            ],
            activity,
        );
        assert_eq!(selected_title(&app), "need");

        handle_key(&mut app, key(KeyCode::Char(']')));
        assert_eq!(selected_title(&app), "active-one");
        handle_key(&mut app, key(KeyCode::Char(']')));
        assert_eq!(selected_title(&app), "old");
        handle_key(&mut app, key(KeyCode::Char(']')));
        assert_eq!(selected_title(&app), "old");

        handle_key(&mut app, key(KeyCode::Char('[')));
        assert_eq!(selected_title(&app), "active-one");
        handle_key(&mut app, key(KeyCode::Char('[')));
        assert_eq!(selected_title(&app), "need");
        handle_key(&mut app, key(KeyCode::Char('[')));
        assert_eq!(selected_title(&app), "need");

        app.selected = 2;
        handle_key(&mut app, key(KeyCode::Char('[')));
        assert_eq!(selected_title(&app), "active-one");
        handle_key(&mut app, key(KeyCode::Char('[')));
        assert_eq!(selected_title(&app), "need");
    }

    #[test]
    fn needs_you_jump_moves_to_the_next_waiting_row_and_wraps() {
        let mut activity = ActivityState::default();
        activity.record_agent_status("p-first", "blocked", NOW - 60);
        activity.record_agent_status("p-second", "done", NOW - 120);
        let mut app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace(
                    "w1",
                    "first",
                    false,
                    None,
                    vec![pane(
                        "p-first",
                        "/repos/first",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
                glance_workspace(
                    "w2",
                    "second",
                    false,
                    None,
                    vec![pane(
                        "p-second",
                        "/repos/second",
                        Some(("claude", AgentState::Done)),
                    )],
                ),
                commit_workspace("w3", "quiet", NOW - 9 * 86_400),
            ],
            activity,
        );

        handle_key(&mut app, shift_key('!'));
        assert_eq!(selected_title(&app), "second");
        handle_key(&mut app, shift_key('!'));
        assert_eq!(selected_title(&app), "first");

        app.selected = 2;
        handle_key(&mut app, shift_key('!'));
        assert_eq!(selected_title(&app), "first");
    }

    #[test]
    fn minus_opens_the_previous_workspace_row() {
        let mut app = glance_app(
            NOW,
            Some("w2"),
            vec![
                glance_workspace("w1", "helm", true, Some(NOW - 60), vec![]),
                glance_workspace("w2", "dots", false, Some(NOW - 120), vec![]),
            ],
            ActivityState::default(),
        );
        app.selected = 1;

        assert_eq!(handle_key(&mut app, key(KeyCode::Char('-'))), Action::Open);
        assert_eq!(selected_title(&app), "dots");
    }

    #[test]
    fn shift_j_and_k_scroll_the_detail_pane_and_selection_resets_it() {
        let mut app = glance_app(
            NOW,
            None,
            vec![
                commit_workspace("w1", "alpha", NOW - 60),
                commit_workspace("w2", "beta", NOW - 120),
            ],
            ActivityState::default(),
        );

        app.detail_max_offset = 10;
        handle_key(&mut app, shift_key('J'));
        handle_key(&mut app, shift_key('J'));
        assert_eq!(app.detail_scroll.offset, 2);
        handle_key(&mut app, shift_key('K'));
        assert_eq!(app.detail_scroll.offset, 1);

        handle_key(&mut app, key(KeyCode::Char('j')));
        app.sync_detail_scroll();
        assert_eq!(app.detail_scroll.offset, 0);

        handle_key(&mut app, shift_key('K'));
        assert_eq!(app.detail_scroll.offset, 0);
    }

    #[test]
    fn label_column_marks_the_first_five_rows_with_pinned_first() {
        let names = [
            "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf",
        ];
        let mut workspaces: Vec<WorkspaceNode> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                commit_workspace(&format!("w{index}"), name, NOW - 60 * (index as i64 + 1))
            })
            .collect();
        workspaces.last_mut().unwrap().label = "golf".into();
        let mut app = glance_app(NOW, None, workspaces, ActivityState::default());
        let golf = app
            .topology_entries
            .iter()
            .find(|entry| entry.title == "golf")
            .unwrap()
            .clone();
        app.pinned_entries.insert(pin_key(&golf));
        app.rebuild_glance();
        app.apply_filter();

        let (lines, _) = render_app(&app, 100, 40);
        let label_at = |name: &str| lines[line_index(&lines, name)].chars().nth(2).unwrap();
        assert_eq!(label_at("golf"), 'a');
        assert_eq!(label_at("alpha"), 's');
        assert_eq!(label_at("bravo"), 'd');
        assert_eq!(label_at("charlie"), 'f');
        assert_eq!(label_at("delta"), 'g');
        assert_eq!(label_at("echo"), ' ');
        assert_eq!(label_at("foxtrot"), ' ');

        handle_key(&mut app, key(KeyCode::Char('/')));
        let (lines, _) = render_app(&app, 100, 40);
        assert_eq!(lines[line_index(&lines, "golf")].chars().nth(2), Some(' '));
    }

    #[test]
    fn header_and_footer_render_at_160_and_100_columns() {
        let mut activity = ActivityState::default();
        activity.record_agent_status("p-skills", "blocked", NOW - 4 * 60);
        activity.record_agent_status("p-writing", "done", NOW - 12 * 60);
        let app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace(
                    "w1",
                    "skills",
                    false,
                    None,
                    vec![pane(
                        "p-skills",
                        "/repos/skills",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
                glance_workspace(
                    "w2",
                    "writing",
                    false,
                    None,
                    vec![pane(
                        "p-writing",
                        "/repos/writing",
                        Some(("claude", AgentState::Done)),
                    )],
                ),
                commit_workspace("w3", "dots", NOW - 14 * 60),
                commit_workspace("w4", "fonts", NOW - 9 * 86_400),
            ],
            activity,
        );

        let (wide, _) = render_app(&app, 160, 40);
        assert!(wide[0].contains("Helm"));
        assert!(wide[0].contains("/ type to search"));
        assert!(wide[0].contains("2 need you"));
        assert!(wide[0].contains("1 active"));
        assert!(wide[0].contains("1 quiet"));
        assert!(!wide[0].contains("recent"));
        assert!(wide[1].contains("^W workspaces"));
        assert!(wide[1].contains("^R roots"));
        assert!(wide[1].contains("^Z zoxide"));
        let footer = wide.last().unwrap();
        assert!(footer.contains("a–g open row"));
        assert!(footer.contains("enter open"));
        assert!(footer.contains("1–9 tab"));
        assert!(footer.contains("] [ group"));
        assert!(footer.contains("! needs you"));
        assert!(footer.contains("/ search"));
        assert!(footer.contains("^B pin"));
        assert!(footer.ends_with("? keys   esc close"));

        let (narrow, _) = render_app(&app, 100, 40);
        let footer = narrow.last().unwrap();
        assert!(footer.contains("a–g open row"));
        assert!(footer.ends_with("? keys   esc close"));
    }

    #[test]
    fn footer_drops_its_left_hints_before_the_right_block() {
        let app = glance_app(
            NOW,
            None,
            vec![commit_workspace("w1", "dots", NOW - 60)],
            ActivityState::default(),
        );
        let (lines, _) = render_app(&app, 80, 40);
        let footer = lines.last().unwrap();
        assert!(footer.contains("a–g open row"));
        assert!(!footer.contains("/ search"));
        assert!(!footer.contains("^B pin"));
        assert!(footer.ends_with("? keys   esc close"));
    }

    #[test]
    fn needs_you_finds_and_counts_pinned_rows_that_wait_on_the_user() {
        let mut activity = ActivityState::default();
        activity.record_agent_status("p-pinned", "blocked", NOW - 60);
        let mut app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace(
                    "w1",
                    "pinned",
                    false,
                    None,
                    vec![pane(
                        "p-pinned",
                        "/repos/pinned",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
                commit_workspace("w2", "quiet", NOW - 9 * 86_400),
            ],
            activity,
        );
        let pinned = app
            .topology_entries
            .iter()
            .find(|entry| entry.title == "pinned")
            .unwrap()
            .clone();
        app.pinned_entries.insert(pin_key(&pinned));
        app.rebuild_glance();
        app.apply_filter();

        app.selected = 1;
        handle_key(&mut app, shift_key('!'));
        assert_eq!(selected_title(&app), "pinned");

        let (lines, _) = render_app(&app, 100, 40);
        assert!(lines[0].contains("1 need you"));
    }

    #[test]
    fn question_mark_sheet_lists_the_sections() {
        let mut app = glance_app(
            NOW,
            None,
            vec![commit_workspace("w1", "dots", NOW - 60)],
            ActivityState::default(),
        );
        handle_key(&mut app, key(KeyCode::Char('?')));
        let (lines, _) = render_app(&app, 100, 40);
        let text = lines.join("\n");
        for section in [" Move", " Open", " Find", " Sources", " Act"] {
            assert!(text.contains(section), "{section}");
        }
        assert!(text.contains("J K"));
        assert!(text.contains("a–g"));
    }

    const WORKSPACE_KEY: &str = "<default>::w1";

    fn subagent(step: &str, done: u32, total: u32, started_at: i64) -> Subagent {
        Subagent {
            step: step.into(),
            done,
            total,
            started_at,
        }
    }

    fn commits() -> Vec<Commit> {
        vec![
            Commit {
                hash: "3f1a9c0".into(),
                subject: "Cycle checkboxes in visual mode".into(),
                committed_at: NOW - 7_200,
            },
            Commit {
                hash: "b82e4d1".into(),
                subject: "Keep indentation on list continuation".into(),
                committed_at: NOW - 86_400,
            },
        ]
    }

    fn dirty_git() -> GitDetail {
        GitDetail {
            changes: vec![
                Change {
                    state: 'M',
                    path: "lua/mdn/lists.lua".into(),
                    added: Some(42),
                    removed: Some(12),
                },
                Change {
                    state: 'A',
                    path: "tests/lists_spec.lua".into(),
                    added: Some(31),
                    removed: Some(0),
                },
                Change {
                    state: '?',
                    path: "notes.md".into(),
                    added: None,
                    removed: None,
                },
            ],
            commits: commits(),
        }
    }

    /// One workspace with three tabs, the first holding `panes`.
    fn detail_app(panes: Vec<PaneNode>, focused_tab: usize, activity: ActivityState) -> App {
        let mut workspace = glance_workspace("w1", "helm.herdr", true, Some(NOW - 360), panes);
        workspace.tabs[0].label = "Agents".into();
        for label in ["Edit", "Shells"] {
            workspace.tabs.push(TabNode {
                id: format!("w1:{label}"),
                label: label.into(),
                focused: false,
                panes: Vec::new(),
            });
        }
        for (index, tab) in workspace.tabs.iter_mut().enumerate() {
            tab.focused = index == focused_tab;
        }
        glance_app(NOW, None, vec![workspace], activity)
    }

    fn claude_pane() -> PaneNode {
        pane(
            "p1",
            "/repos/helm.herdr",
            Some(("claude", AgentState::Working)),
        )
    }

    #[test]
    fn detail_sections_keep_their_height_for_clean_and_dirty_repos() {
        let mut clean = detail_app(vec![claude_pane()], 0, ActivityState::default());
        clean.cache_git(
            WORKSPACE_KEY.into(),
            GitDetail {
                changes: Vec::new(),
                commits: commits(),
            },
            NOW,
        );
        let mut dirty = detail_app(vec![claude_pane()], 0, ActivityState::default());
        dirty.cache_git(WORKSPACE_KEY.into(), dirty_git(), NOW);

        let (clean_lines, _) = render_app(&clean, 180, 45);
        let (dirty_lines, _) = render_app(&dirty, 180, 45);
        assert_eq!(
            line_index(&clean_lines, "RECENT COMMITS"),
            line_index(&dirty_lines, "RECENT COMMITS")
        );
        assert!(clean_lines.iter().any(|line| line.contains("clean")));
        assert!(dirty_lines
            .iter()
            .any(|line| line.contains("CHANGES · 3 files")));
    }

    #[test]
    fn detail_pane_hides_below_110_columns() {
        let app = detail_app(vec![claude_pane()], 0, ActivityState::default());
        let (narrow, _) = render_app(&app, 109, 40);
        assert!(!narrow.iter().any(|line| line.contains('│')));
        let (wide, _) = render_app(&app, 110, 40);
        assert!(wide.iter().any(|line| line.contains('│')));
    }

    #[test]
    fn tab_chips_do_not_move_when_the_focused_tab_changes() {
        let first = render_app(
            &detail_app(vec![claude_pane()], 0, ActivityState::default()),
            180,
            45,
        )
        .0;
        let second = render_app(
            &detail_app(vec![claude_pane()], 1, ActivityState::default()),
            180,
            45,
        )
        .0;
        assert_eq!(first, second);
        assert!(first.iter().any(|line| line.contains("1  Agents")));
    }

    #[test]
    fn list_progress_shows_only_for_workspaces_with_a_working_agent() {
        let mut app = glance_app(
            NOW,
            None,
            vec![
                glance_workspace(
                    "w1",
                    "helm",
                    true,
                    None,
                    vec![pane(
                        "p-helm",
                        "/repos/helm",
                        Some(("claude", AgentState::Working)),
                    )],
                ),
                glance_workspace(
                    "w2",
                    "skills",
                    false,
                    None,
                    vec![pane(
                        "p-skills",
                        "/repos/skills",
                        Some(("claude", AgentState::Blocked)),
                    )],
                ),
            ],
            ActivityState::default(),
        );
        for id in ["p-helm", "p-skills"] {
            app.subagents
                .insert(id.into(), vec![subagent("Port the tests", 3, 6, NOW - 192)]);
        }
        let (lines, _) = render_app(&app, 100, 20);
        assert!(lines[line_index(&lines, "helm")].contains("████░░░░"));
        assert!(!lines[line_index(&lines, "skills")].contains('█'));
    }

    #[test]
    fn list_progress_column_shows_summed_subagent_progress() {
        let mut app = detail_app(vec![claude_pane()], 0, ActivityState::default());
        app.subagents.insert(
            "p1".into(),
            vec![subagent("Port the tests", 3, 6, NOW - 192)],
        );
        let (lines, _) = render_app(&app, 100, 40);
        assert!(lines[line_index(&lines, "helm.herdr")].contains("████░░░░"));
    }

    #[test]
    fn glance_detail_renders_agents_changes_and_commits_at_180_by_45() {
        let mut agent = claude_pane();
        agent.title = Some("Port the glance detail pane".into());
        let mut activity = ActivityState::default();
        activity.record_agent_status("p1", "working", NOW - 14 * 60);
        let mut app = detail_app(vec![agent], 0, activity);
        app.subagents.insert(
            "p1".into(),
            vec![
                subagent("Port the list continuation tests", 4, 6, NOW - 192),
                subagent("Research concealed list markers", 1, 4, NOW - 48),
            ],
        );
        app.cache_git(WORKSPACE_KEY.into(), dirty_git(), NOW);

        let (lines, _) = render_app(&app, 180, 45);
        assert!(line_index(&lines, "AGENTS") < line_index(&lines, "CHANGES · 3 files"));
        assert!(line_index(&lines, "CHANGES · 3 files") < line_index(&lines, "RECENT COMMITS"));
        assert!(lines
            .iter()
            .any(|line| line.contains("└ Port the list continuation tests")));
        assert!(lines.iter().any(|line| line.contains("1  Agents")));
        if let Ok(path) = std::env::var("HELM_GLANCE_DUMP") {
            std::fs::write(path, lines.join("\n")).unwrap();
        }
    }
}
