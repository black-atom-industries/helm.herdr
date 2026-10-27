use std::{
    io,
    path::{Path, PathBuf},
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
    layout::{Constraint, Direction, Layout, Margin, Rect},
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
    glance::{self, Group, IconStyle},
    keymap::{keybindings, Command},
    model::{workspace_key, Entry, EntryAction, Source},
    paths::home,
    sessions::{Session, SessionState, SwitchError},
    sources::status_icon_at,
    theme::Theme,
    topology::{
        self, repository_color, AgentState, GitStatus, OpenTopology, PaneNode, WorkspaceNode,
    },
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
    let mut session_git_job: Option<(String, Receiver<OpenTopology>)> = None;
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
        if let Some(workspaces) = workspace_refresh.try_iter().last() {
            app.apply_workspaces(workspaces, now_unix_secs());
        }
        app.sync_detail_scroll();
        drive_detail(app, &mut detail_job, now_unix_secs());
        drive_session_git(app, &mut session_git_job, now_unix_secs());
        let mut detail_max = 0;
        terminal.draw(|f| {
            if !persist && too_small(f.area()) {
                draw_too_small(f, app);
                list_hits = ListHits::default();
                return;
            }
            let (hits, max) = draw(f, app);
            list_hits = hits;
            detail_max = max;
        })?;
        app.detail_max_offset = detail_max;
        let animate = has_working_entry(app) || app.glance_view() || app.sessions_view();
        let frame = if animate
            || update_check.is_some()
            || detail_job.is_some()
            || session_git_job.is_some()
        {
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
            Action::Switched => {
                if !persist {
                    break Ok(());
                }
            }
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

/// Keeps at most one git read of another session's Workspaces in flight for
/// the Sessions view, and stores its result once it arrives.
fn drive_session_git(app: &mut App, job: &mut Option<(String, Receiver<OpenTopology>)>, now: i64) {
    match job.take() {
        Some((label, receiver)) => match receiver.try_recv() {
            Ok(topology) => {
                app.session_git.insert(label, (topology, now));
            }
            Err(TryRecvError::Empty) => *job = Some((label, receiver)),
            Err(TryRecvError::Disconnected) => {}
        },
        None => {
            let Some((label, mut topology)) = app.session_git_target(now) else {
                return;
            };
            let (sender, receiver) = mpsc::channel();
            thread::spawn(move || {
                topology::enrich_git(&mut topology, &topology::NativeGitProbe);
                let _ = sender.send(topology);
            });
            *job = Some((label, receiver));
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

/// Reloads the snapshot and activity every few seconds. Git identities are
/// read again only once `GIT_REFRESH` has passed; until then they come from
/// the last load.
fn spawn_workspace_refresh(config: Config, mut known: OpenTopology) -> Receiver<Workspaces> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut git_read_at = Instant::now();
        loop {
            thread::sleep(WORKSPACE_REFRESH);
            let reuse_git = git_read_at.elapsed() < GIT_REFRESH;
            let workspaces = load_workspaces(&config, reuse_git.then_some(&known), None);
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
    /// A launcher is switching this terminal to another Herdr session.
    Switched,
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
    app.notice = None;
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
                return stay_for_other_session(app, Action::Open);
            }
            app.selected = *row;
        }
        _ => {}
    }
    Action::Continue
}

fn handle_key(app: &mut App, key: KeyEvent) -> Action {
    app.notice = None;
    let action = key_action(app, key);
    stay_for_other_session(app, action)
}

/// Opening a Workspace of another Herdr session switches sessions through the
/// launcher, or keeps the Picker open with a notice when none runs. In the
/// Sessions view, opening the current session's row returns to the glance,
/// and its numbered Workspaces open here.
fn stay_for_other_session(app: &mut App, action: Action) -> Action {
    let number = match action {
        Action::Open | Action::OpenTemplate => None,
        Action::OpenTab(number) => Some(number),
        _ => return action,
    };
    if app.sessions_view() {
        return match app.session_action(number) {
            Some(EntryAction::SwitchSession {
                session,
                workspace,
                tab,
            }) => switch(app, session.as_deref(), workspace, tab, None),
            Some(_) => action,
            None if number.is_none() => {
                app.set_filter(None);
                app.apply_filter();
                Action::Continue
            }
            None => Action::Continue,
        };
    }
    let Some(EntryAction::FocusWorkspace { session, id }) = app
        .selected_entry()
        .filter(|entry| app.is_elsewhere(entry))
        .map(|entry| entry.action.clone())
    else {
        return action;
    };
    let (tab, pane) = match number {
        Some(number) => match app.selected_tab_action(number) {
            Some(EntryAction::FocusTab { id, .. }) => (Some(id), None),
            _ => (None, None),
        },
        None => app
            .selected_entry()
            .and_then(|entry| app.needs_you_pane(entry))
            .map_or((None, None), |(tab, pane)| (Some(tab), Some(pane))),
    };
    switch(app, session.as_deref(), Some(id), tab, pane)
}

/// Every successful switch closes the popup: its terminal now shows another session.
fn switch(
    app: &mut App,
    session: Option<&str>,
    workspace: Option<String>,
    tab: Option<String>,
    pane: Option<String>,
) -> Action {
    if app.switch_or_notice(session, workspace, tab, pane) {
        Action::Switched
    } else {
        Action::Continue
    }
}

fn key_action(app: &mut App, key: KeyEvent) -> Action {
    let previous_label = app.last_label.take();
    let close_armed = std::mem::take(&mut app.close_armed);
    let cold_armed = std::mem::take(&mut app.cold_armed);
    let command = keybindings(app)
        .into_iter()
        .find_map(|binding| binding.command_for(app, key));

    if app.input_mode == InputMode::Help {
        if matches!(command, Some(Command::Back | Command::ToggleHelp))
            || key.code == KeyCode::Char('q')
        {
            app.input_mode = InputMode::Normal;
        }
        return Action::Continue;
    }

    if let Some(command) = command {
        // Workspace ids repeat across sessions, so these would act on a local
        // namesake, and a session row is no Workspace.
        if matches!(
            command,
            Command::ArmClose | Command::CloseWorkspace | Command::ToggleCold | Command::ToggleMark
        ) && app
            .selected_entry()
            .is_some_and(|entry| app.is_elsewhere(entry) || entry.source == Source::Session)
        {
            return Action::Continue;
        }
        if command == Command::ArmClose {
            if close_armed {
                return Action::CloseWorkspace;
            }
            app.close_armed = app.selected_entry().is_some();
            return Action::Continue;
        }
        if command == Command::ToggleCold
            && !cold_armed
            && app
                .selected_entry()
                .is_some_and(|entry| !app.is_cold(entry))
        {
            app.cold_armed = true;
            return Action::Continue;
        }
        if command == Command::SelectLabel {
            return select_label(app, key, previous_label);
        }
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

fn select_label(app: &mut App, key: KeyEvent, previous_label: Option<char>) -> Action {
    let KeyCode::Char(ch) = key.code else {
        return Action::Continue;
    };
    let slot = app
        .config
        .glance
        .labels()
        .iter()
        .position(|label| *label == ch);
    let Some(position) = slot.and_then(|slot| app.labelled_positions().get(slot).copied()) else {
        return Action::Continue;
    };
    app.selected = position;
    if previous_label == Some(ch) {
        return Action::Open;
    }
    app.last_label = Some(ch);
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
        Command::SelectLabel => select_label(app, key, None),
        Command::OpenTab => match key.code {
            KeyCode::Char(ch) if ('1'..='9').contains(&ch) => {
                let number = usize::from(ch as u8 - b'0');
                if app.sessions_view() || app.selected_tab_action(number).is_some() {
                    Action::OpenTab(number)
                } else {
                    Action::Continue
                }
            }
            _ => Action::Continue,
        },
        Command::OpenPrevious => {
            if let Some((session, workspace)) = app.previous_elsewhere() {
                match app.switch_session(session.as_deref(), Some(workspace), None, None) {
                    Ok(()) => return Action::Switched,
                    Err(SwitchError::Failed(_)) => return Action::Continue,
                    Err(SwitchError::NoLauncher) => {}
                }
            }
            if app.sessions_view() && app.previous_workspace_id.is_some() {
                app.set_filter(None);
                app.apply_filter();
            }
            match app.previous_position() {
                Some(position) => {
                    app.selected = position;
                    Action::Open
                }
                None => Action::Continue,
            }
        }
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
        Command::ArmClose => Action::Continue,
        Command::ToggleMark => {
            if let Err(error) = app.toggle_selected_pin() {
                crate::herdr::notify_error(
                    &format!("Mark failed: {error}"),
                    &app.config.notifications,
                );
            }
            Action::Continue
        }
        Command::ToggleCold => {
            if let Err(error) = app.toggle_selected_cold() {
                crate::herdr::notify_error(
                    &format!("Cold failed: {error}"),
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
    let full = f.area();
    f.render_widget(Clear, full);
    f.render_widget(
        Block::default().style(Style::default().bg(app.theme.popup_background())),
        full,
    );
    let area = content_area(full);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);

    let bar = Block::default().style(Style::default().bg(app.theme.popup_background()));
    f.render_widget(
        bar.clone(),
        Rect {
            height: rows[0].bottom().saturating_sub(full.y),
            ..full
        },
    );
    f.render_widget(
        bar,
        Rect {
            y: rows[2].y,
            height: full.bottom().saturating_sub(rows[2].y),
            ..full
        },
    );
    draw_header(f, app, indent(rows[0]));
    let (list_hits, detail_max) = if app.glance_view() || app.sessions_view() {
        draw_glance(f, app, rows[1])
    } else {
        (draw_list(f, app, rows[1]), 0)
    };

    draw_key_hints(f, app, indent(rows[2]));
    if app.input_mode == InputMode::Help {
        draw_keybindings_help(f, app, area);
    }
    (list_hits, detail_max)
}

const HEADER_COUNTS_MIN_WIDTH: u16 = 90;

/// The smallest popup the glance and its detail pane fit in.
const POPUP_MIN_WIDTH: u16 = 120;
const POPUP_MIN_HEIGHT: u16 = 32;

fn too_small(area: Rect) -> bool {
    area.width < POPUP_MIN_WIDTH || area.height < POPUP_MIN_HEIGHT
}

fn draw_too_small(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default().style(Style::default().bg(app.theme.popup_background())),
        area,
    );
    let message = format!(
        "Helm needs at least {POPUP_MIN_WIDTH} × {POPUP_MIN_HEIGHT}, this popup is {} × {}",
        area.width, area.height
    );
    let hint = "Set width and height in cells on the Herdr popup binding · q quits";
    let y = area.y + area.height / 2;
    for (offset, (text, style)) in [
        (message, Style::default().fg(app.theme.text)),
        (hint.to_string(), Style::default().fg(app.theme.subtext0)),
    ]
    .into_iter()
    .enumerate()
    {
        let row = Rect {
            y: (y + offset as u16).min(area.bottom().saturating_sub(1)),
            height: 1,
            ..area
        };
        f.render_widget(
            Paragraph::new(text)
                .style(style)
                .alignment(ratatui::layout::Alignment::Center),
            row,
        );
    }
}

/// Header and footer text starts where the list text does, inside the card.
fn indent(area: Rect) -> Rect {
    Rect {
        x: area.x + 1.min(area.width),
        width: area.width.saturating_sub(2),
        ..area
    }
}

/// Herdr's popup frame keeps one column on the right for its scrollbar, so
/// the left inset is one column wider to look even.
fn content_area(full: Rect) -> Rect {
    Rect {
        x: full.x + 2.min(full.width),
        y: full.y + 1.min(full.height),
        width: full.width.saturating_sub(3),
        height: full.height.saturating_sub(2),
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let inner = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    let lines = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    f.render_widget(Paragraph::new(Line::from(session_spans(app))), lines[1]);

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
        lines[2],
    );
}

fn session_spans(app: &App) -> Vec<Span<'static>> {
    let icon = match app.config.glance.icons {
        IconStyle::Nerd => "\u{F0328}",
        IconStyle::Unicode => "◆",
    };
    let style = Style::default().fg(app.theme.accent);
    let mut spans = vec![
        Span::styled(icon, style),
        Span::raw(" "),
        Span::styled(
            app.session.clone().unwrap_or_else(|| "default".into()),
            style.add_modifier(Modifier::BOLD),
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
    spans
}

fn header_spans(app: &App) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        "Helm",
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    )];
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
                .filter(|row| row.group == Group::Hot)
                .count(),
            "hot",
            Group::Hot,
        ),
        (
            app.glance
                .values()
                .filter(|row| row.group == Group::Warm)
                .count(),
            "warm",
            Group::Warm,
        ),
        (
            app.glance
                .values()
                .filter(|row| row.group == Group::Cold)
                .count(),
            "cold",
            Group::Cold,
        ),
    ];
    let elsewhere = app
        .glance
        .values()
        .filter(|row| row.needs_you && row.other_session.is_some())
        .count();
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
        if group == Group::NeedsYou && elsewhere > 0 {
            spans.push(Span::styled(
                format!(" · {elsewhere} elsewhere"),
                Style::default().fg(app.theme.subtext0),
            ));
        }
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

fn hint_width(hints: &[(String, String)]) -> usize {
    hints
        .iter()
        .map(|(key, label)| Span::raw(key.as_str()).width() + 1 + Span::raw(label.as_str()).width())
        .sum::<usize>()
        + HINT_GAP.len() * hints.len().saturating_sub(1)
}

fn hint_line(app: &App, hints: &[(String, String)]) -> Line<'static> {
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
    let area = Rect {
        y: area.y + 1.min(area.height),
        height: area.height.saturating_sub(1),
        ..area
    };
    let armed = if app.close_armed {
        Some(("x", "close", app.theme.red))
    } else if app.cold_armed {
        Some(("c", "send to cold", app.theme.blue))
    } else {
        None
    };
    if let Some(notice) = app.notice.clone().filter(|_| armed.is_none()) {
        f.render_widget(
            Paragraph::new(Span::styled(notice, Style::default().fg(app.theme.peach)))
                .style(Style::default().bg(app.theme.popup_background())),
            area,
        );
        return;
    }
    if let Some((key, verb, color)) = armed {
        let name = app.selected_entry().map(display_title).unwrap_or_default();
        let line = Line::from(vec![
            Span::styled(key, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" again to {verb} {name} · any other key cancels"),
                Style::default().fg(color),
            ),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(app.theme.popup_background())),
            area,
        );
        return;
    }
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
    let style = Style::default().bg(app.theme.popup_background());
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
    let column = |groups: &[&str]| {
        let mut lines = Vec::new();
        for group in groups {
            let start = lines.len();
            lines.push(Line::styled(
                format!(" {group}"),
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
            for binding in bindings.iter().filter(|binding| binding.group == *group) {
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
                    Span::styled(key.clone(), key_style),
                    Span::raw(" ".repeat(12usize.saturating_sub(key.chars().count()))),
                    Span::styled(binding.label.clone(), Style::default().fg(app.theme.text)),
                ]));
            }
            if lines.len() == start + 1 {
                lines.pop();
            } else {
                lines.push(Line::default());
            }
        }
        lines.pop();
        lines
    };
    let left = column(&["Move", "Open", "Find"]);
    let right = column(&["Sources", "Act"]);

    let rows = left.len().max(right.len()) as u16;
    let height = (rows + 2).min(area.height.saturating_sub(2).max(1));
    let popup = area.centered(Constraint::Percentage(90), Constraint::Length(height));
    let block = Block::default()
        .title(" Keybindings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.accent))
        .style(Style::default().bg(app.theme.panel_bg));
    let inner = block.inner(popup);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);
    f.render_widget(Paragraph::new(Text::from(left)), columns[0]);
    f.render_widget(Paragraph::new(Text::from(right)), columns[1]);
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
    home_relative(&entry.path)
}

fn home_relative(path: &Path) -> String {
    path.strip_prefix(home())
        .ok()
        .map(|relative| {
            if relative.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", relative.display())
            }
        })
        .unwrap_or_else(|| path.display().to_string())
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

/// Columns besides name, branch and signal: margin, label and icon, a
/// one-column gap after label, a two-column gap after icon, name, branch and
/// signal, and the progress bar.
const GLANCE_FIXED_WIDTH: usize = 1 + 3 + 1 + 1 + 4 * 2 + 8;
const GLANCE_PROGRESS_WIDTH: usize = 8;
const GLANCE_SIGNAL_MAX: usize = 24;
const GLANCE_GIT_MAX: usize = 12;

struct GlanceCells {
    icon: &'static str,
    icon_color: Color,
    name: String,
    /// The `@session` tag of a Workspace in another Herdr session, or a
    /// session's `N need you`.
    tag: Option<String>,
    tag_color: Color,
    name_color: Color,
    branch: String,
    git: Option<GitStatus>,
    signal: String,
}

fn glance_cells(app: &App, entry: &Entry) -> GlanceCells {
    if let Some(state) = app.session_state(entry) {
        return session_cells(app, entry, state);
    }
    match app.glance_row(entry) {
        Some(row) => GlanceCells {
            icon: glance::status_icon(row.state, app.config.glance.icons, app.spinner_tick),
            icon_color: agent_state_color(&app.theme, row.state),
            name: row.name.clone(),
            tag: row
                .other_session
                .as_ref()
                .map(|session| format!("@{session}")),
            tag_color: app.theme.mauve,
            name_color: repository_color(&row.repository, &[], &app.theme),
            branch: row.branch.clone(),
            git: app
                .workspace_node(entry)
                .and_then(|workspace| workspace.git.as_ref())
                .and_then(|git| git.status),
            signal: glance::signal_text(&row.signal, app.now),
        },
        None => GlanceCells {
            icon: "",
            icon_color: app.theme.text,
            name: display_title(entry).to_string(),
            tag: None,
            tag_color: app.theme.mauve,
            name_color: app.theme.text,
            branch: display_path(entry),
            git: None,
            signal: String::new(),
        },
    }
}

/// A Sessions view row: the session's most urgent icon, its name with how many
/// Workspaces need you, its Workspace count and the age of its newest activity.
/// A stopped session shows a dim name and `start`.
fn session_cells(app: &App, entry: &Entry, state: &SessionState) -> GlanceCells {
    let summary = app.session_summary(state);
    let running = state.topology.is_some();
    let icon_state = summary.state.unwrap_or(AgentState::Unknown);
    let tag_state = summary
        .needs_you
        .map_or(AgentState::Unknown, |(state, _)| state);
    GlanceCells {
        icon: glance::status_icon(icon_state, app.config.glance.icons, app.spinner_tick),
        icon_color: agent_state_color(&app.theme, icon_state),
        name: entry.title.clone(),
        tag: summary
            .needs_you
            .map(|(_, count)| format!("{count} need you")),
        tag_color: agent_state_color(&app.theme, tag_state),
        name_color: if running {
            app.theme.text
        } else {
            app.theme.subtext0
        },
        branch: if running {
            format!("{} ws", summary.workspaces)
        } else {
            "start".into()
        },
        git: None,
        signal: summary
            .last_active
            .map(|at| glance::age(app.now, at))
            .unwrap_or_default(),
    }
}

/// The heading of the group a row sits in: its Warmth group in the glance,
/// RUNNING or STOPPED in the Sessions view.
fn row_heading(app: &App, entry: &Entry) -> Option<(String, Color)> {
    if let Some(state) = app.session_state(entry) {
        return Some(if state.topology.is_some() {
            ("RUNNING".into(), app.theme.accent)
        } else {
            ("STOPPED".into(), app.theme.subtext0)
        });
    }
    let group = app.glance_group(entry)?;
    Some((group.label().to_uppercase(), group_color(&app.theme, group)))
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
        Group::Hot => theme.yellow,
        Group::Warm => theme.green,
        Group::Cold => theme.overlay0,
        Group::Directories | Group::Other => theme.subtext0,
    }
}

/// The width of a name cell: the name, then a space and its tag.
fn name_cell_width(cells: &GlanceCells) -> usize {
    Span::raw(&cells.name).width()
        + cells
            .tag
            .as_ref()
            .map_or(0, |tag| 1 + Span::raw(tag).width())
}

/// The name, then its tag, in `width` cells. A short cell clips the name and
/// keeps the tag whole while the name keeps at least two cells, or all of a
/// shorter name.
fn name_cell(cells: &GlanceCells, width: usize, name: Style, tag: Style) -> Vec<Span<'static>> {
    let Some(tag_text) = &cells.tag else {
        return vec![Span::styled(fit_cell(&cells.name, width), name)];
    };
    let tag_width = Span::raw(tag_text).width();
    let name_room = width.saturating_sub(tag_width + 1);
    if name_room < Span::raw(&cells.name).width().min(2) {
        return vec![Span::styled(fit_cell(&cells.name, width), name)];
    }
    let clipped = truncate_terminal(&cells.name, name_room);
    let padding = width.saturating_sub(Span::raw(&clipped).width() + 1 + tag_width);
    vec![
        Span::styled(clipped, name),
        Span::raw(" "),
        Span::styled(tag_text.clone(), tag),
        Span::raw(" ".repeat(padding)),
    ]
}

/// The GIT cell's parts: `+n` staged, `!n` modified, `?n` untracked, `↑n`
/// ahead and `↓n` behind, or `clean`. Empty while the status is unknown.
fn git_parts(app: &App, status: Option<GitStatus>) -> Vec<(String, Color)> {
    let Some(status) = status else {
        return Vec::new();
    };
    let (ahead, behind) = status.ahead_behind.unwrap_or((0, 0));
    let parts: Vec<(String, Color)> = [
        ('+', status.staged, app.theme.green),
        ('!', status.modified, app.theme.yellow),
        ('?', status.untracked, app.theme.subtext0),
        ('↑', ahead, app.theme.accent),
        ('↓', behind, app.theme.red),
    ]
    .into_iter()
    .filter(|(_, count, _)| *count > 0)
    .map(|(symbol, count, color)| (format!("{symbol}{count}"), color))
    .collect();
    if parts.is_empty() {
        vec![("✓".into(), app.theme.green)]
    } else {
        parts
    }
}

fn git_width(parts: &[(String, Color)]) -> usize {
    parts
        .iter()
        .map(|(text, _)| Span::raw(text).width() + 1)
        .sum::<usize>()
        .saturating_sub(1)
}

/// The parts joined by a space in `width` cells, cut from the right.
fn git_cell(parts: &[(String, Color)], width: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used = 0;
    for (index, (text, color)) in parts.iter().enumerate() {
        let gap = usize::from(index > 0);
        let room = width.saturating_sub(used + gap);
        if room == 0 {
            break;
        }
        let clipped = truncate_terminal(text, room);
        used += gap + Span::raw(&clipped).width();
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(clipped, Style::default().fg(*color)));
    }
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    spans
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
        let card = list_card(f, app, area);
        let list = draw_legend(f, app, card);
        return (draw_glance_list(f, app, list), 0);
    }
    let [list, border, detail] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Length(2),
        Constraint::Percentage(50),
    ])
    .areas(area);
    let _ = border;
    f.render_widget(
        Block::default().style(Style::default().bg(app.theme.chrome_background())),
        detail,
    );
    let max_offset = if app.sessions_view() {
        draw_session_detail(f, app, detail)
    } else {
        draw_detail(f, app, detail)
    };
    let card = list_card(f, app, list);
    let list = draw_legend(f, app, card);
    (draw_glance_list(f, app, list), max_offset)
}

/// Draws the icon legend on the last row of the list card, dropping entries
/// from the right that do not fit, and returns the rows above it.
fn draw_legend(f: &mut Frame, app: &App, card: Rect) -> Rect {
    let [list, legend] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(card);
    let icons = app.config.glance.icons;
    let state = |state: AgentState, word| {
        (
            glance::status_icon(state, icons, app.spinner_tick),
            agent_state_color(&app.theme, state),
            word,
        )
    };
    let entries = [
        state(AgentState::Working, "working"),
        state(AgentState::Blocked, "waits"),
        state(AgentState::Done, "done"),
        state(AgentState::Idle, "idle"),
    ];
    let dim = Style::default().fg(app.theme.subtext0);
    let mut spans = vec![Span::raw(" ")];
    let mut used = 1;
    for (glyph, color, word) in entries {
        let gap = if spans.len() > 1 { 2 } else { 0 };
        let width = gap + Span::raw(glyph).width() + 2 + word.len();
        if used + width > legend.width as usize {
            break;
        }
        if gap > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(glyph, Style::default().fg(color)));
        spans.push(Span::styled(format!("  {word}"), dim));
        used += width;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), legend);
    list
}

/// Paints the list on the base background inside the chrome and returns the
/// padded area its rows use.
fn list_card(f: &mut Frame, app: &App, area: Rect) -> Rect {
    f.render_widget(
        Block::default().style(Style::default().bg(app.theme.bar_background())),
        area,
    );
    area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
}

fn draw_glance_list(f: &mut Frame, app: &App, area: Rect) -> ListHits {
    let sessions_view = app.sessions_view();
    let visible: Vec<(usize, (String, Color), GlanceCells)> = app
        .filtered
        .iter()
        .enumerate()
        .filter_map(|(position, index)| {
            let entry = &app.entries[*index];
            let heading = row_heading(app, entry)?;
            Some((position, heading, glance_cells(app, entry)))
        })
        .collect();

    let (state_header, name_header, branch_header) = if sessions_view {
        ("", "SESSION", "WORKSPACES")
    } else {
        ("ST", "WORKSPACE", "BRANCH")
    };
    let name_width = visible
        .iter()
        .map(|(_, _, cells)| name_cell_width(cells))
        .max()
        .unwrap_or(0)
        .max(name_header.len());
    let branch_width = visible
        .iter()
        .map(|(_, _, cells)| Span::raw(&cells.branch).width())
        .max()
        .unwrap_or(0)
        .max(branch_header.len());
    let signal_width = visible
        .iter()
        .map(|(_, _, cells)| Span::raw(&cells.signal).width())
        .max()
        .unwrap_or(0)
        .clamp("AGE".len(), GLANCE_SIGNAL_MAX);
    let git_width = visible
        .iter()
        .map(|(_, _, cells)| git_width(&git_parts(app, cells.git)))
        .max()
        .unwrap_or(0)
        .clamp("GIT".len(), GLANCE_GIT_MAX);
    let git_room = if sessions_view { 0 } else { git_width + 2 };
    let flex = (area.width as usize).saturating_sub(GLANCE_FIXED_WIDTH + signal_width + git_room);
    let name_width = name_width.min(flex * 3 / 5);
    let branch_width = branch_width.min(flex.saturating_sub(name_width));

    let area = if visible.is_empty() {
        area
    } else {
        let [header, _, rows] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);
        let mut columns = vec![
            " KEY ".into(),
            fit_cell(state_header, 3),
            fit_cell(name_header, name_width),
            "  ".into(),
            fit_cell(branch_header, branch_width),
            "  ".into(),
        ];
        if !sessions_view {
            columns.extend([fit_cell("GIT", git_width), "  ".into()]);
        }
        columns.push(fit_cell("AGE", signal_width));
        if !sessions_view {
            columns.extend(["  ".into(), "PROGRESS".into()]);
        }
        f.render_widget(
            Paragraph::new(Line::styled(
                columns.concat(),
                Style::default()
                    .fg(app.theme.subtext0)
                    .add_modifier(Modifier::BOLD),
            )),
            header,
        );
        rows
    };

    let labels = app.config.glance.labels();
    let labelled = if app.input_mode == InputMode::Normal {
        app.labelled_positions()
    } else {
        Vec::new()
    };
    let mut items = Vec::new();
    let mut hit_rows = Vec::new();
    let mut selected_item = None;
    let mut selected_labelled = false;
    let mut current: Option<&str> = None;
    for (position, (heading, heading_color), cells) in &visible {
        if current != Some(heading.as_str()) {
            if current.is_some() {
                items.push(ListItem::new(Line::raw("")));
                hit_rows.push(None);
            }
            items.push(ListItem::new(Line::from(Span::styled(
                format!(" {heading}"),
                Style::default()
                    .fg(*heading_color)
                    .add_modifier(Modifier::BOLD),
            ))));
            hit_rows.push(None);
            current = Some(heading.as_str());
        }
        if *position == app.selected {
            selected_item = Some(items.len());
        }
        let label = labelled
            .iter()
            .position(|&slot| slot == *position)
            .and_then(|slot| labels.get(slot));
        if *position == app.selected {
            selected_labelled = label.is_some();
        }
        let label = match label {
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
        ];
        spans.extend(name_cell(
            cells,
            name_width,
            Style::default()
                .fg(cells.name_color)
                .add_modifier(Modifier::BOLD),
            Style::default().fg(cells.tag_color),
        ));
        spans.extend([
            Span::raw("  "),
            Span::styled(
                fit_cell(&cells.branch, branch_width),
                Style::default().fg(app.theme.subtext0),
            ),
            Span::raw("  "),
        ]);
        if !sessions_view {
            spans.extend(git_cell(&git_parts(app, cells.git), git_width));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            fit_cell(&cells.signal, signal_width),
            Style::default().fg(app.theme.subtext0),
        ));
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
    if let Some(line) = selected_item.filter(|&line| line >= state.offset()) {
        let y = area.y + (line - state.offset()) as u16;
        if y < area.bottom() && area.width > 1 {
            let accent = Style::default().fg(app.theme.accent);
            let buf = f.buffer_mut();
            buf[(area.x, y)].set_symbol("▌").set_style(accent);
            buf[(area.right() - 1, y)].set_symbol("▐").set_style(accent);
            if selected_labelled {
                for x in (area.x + 1..area.x + 4).filter(|&x| x < area.right() - 1) {
                    buf[(x, y)].set_bg(app.theme.surface0);
                }
            }
        }
    }

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

/// `▰` for the done share and `▱` for the rest, rounded to the nearest cell.
fn bar(done: u32, total: u32, cells: usize) -> String {
    let filled = if total == 0 {
        0
    } else {
        let done = done.min(total) as usize;
        (done * cells + total as usize / 2) / total as usize
    };
    format!("{}{}", "▰".repeat(filled), "▱".repeat(cells - filled))
}

/// Draws the detail pane and returns the largest useful scroll offset. The header
/// and agents sit at the top, and the changes and commits sections are anchored
/// to the bottom. Their height depends only on the pane height unless the
/// agents need the room, and never drops below `DETAIL_SECTION_MIN_ROWS` for
/// the sake of subagents.
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
        y: area.y + 1.min(area.height),
        width: area.width.saturating_sub(3),
        height: area.height.saturating_sub(2),
    };
    let width = area.width as usize;
    let dim = Style::default().fg(app.theme.subtext0);

    let chips = fit_chips(tab_chips(app, workspace), width);
    let name_room = width;
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
        Line::from(chips),
        Line::raw(""),
        Line::from(left),
        Line::styled(truncate_end(&display_path(entry), width), dim),
        Line::styled(truncate_end(&activity_sources(app, workspace), width), dim),
        Line::raw(""),
    ];

    let git = app.git_detail.get(&key).map(|cached| &cached.detail);
    let changes: &[Change] = git.map_or(&[], |git| &git.changes);
    let commits: &[Commit] = git.map_or(&[], |git| &git.commits);
    // Each section is a heading plus `rows` lines, with a blank line between them.
    let height = area.height as usize;
    let section_lines = |rows: usize| 3 + 2 * rows;
    let agents = agent_lines(
        app,
        workspace,
        width,
        height.saturating_sub(header.len() + 1 + section_lines(DETAIL_SECTION_MIN_ROWS)),
    );
    let pane_rows = (height.saturating_sub(header.len() + 3) / 2)
        .clamp(DETAIL_SECTION_MIN_ROWS, DETAIL_SECTION_MAX_ROWS);
    // One blank line always separates the agents from the changes.
    let free_rows = height.saturating_sub(header.len() + agents.len() + 4) / 2;
    let rows = pane_rows.min(free_rows).max(1);
    let lower_height = section_lines(rows).min(height) as u16;
    let top_height = (header.len() + agents.len() + 1) as u16;
    // With room, the lower sections sit at the bottom so they never move; without
    // it, they follow the agents and lose their last rows instead.
    let [top, bottom] = if top_height + lower_height <= area.height {
        Layout::vertical([Constraint::Min(0), Constraint::Length(lower_height)]).areas(area)
    } else {
        Layout::vertical([Constraint::Length(top_height), Constraint::Min(0)]).areas(area)
    };

    let mut top_lines = header;
    top_lines.extend(agents);
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
        .focused_at(workspace.session.as_deref(), &workspace.id)
        .map(|focused_at| format!("focused {}", glance::age(app.now, focused_at)));
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
                .fg(app.theme.popup_background())
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

/// Drops whole chips from the right until the chips fit in `room` cells.
fn fit_chips(mut spans: Vec<Span<'static>>, room: usize) -> Vec<Span<'static>> {
    while spans.iter().map(|span| span.width()).sum::<usize>() > room {
        // A chip is a number and a label, and every chip after the first follows a gap.
        spans.truncate(spans.len().saturating_sub(3));
    }
    spans
}

/// The AGENTS section in at most `max_lines` lines. The heading and every
/// agent always show; subagents fill the lines that are left, and the rest
/// collapse into a `+N more` line.
fn agent_lines(
    app: &App,
    workspace: &WorkspaceNode,
    width: usize,
    max_lines: usize,
) -> Vec<Line<'static>> {
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
    let mut spare = max_lines.saturating_sub(1 + panes.len());
    let mut lines = vec![Line::styled("AGENTS", dim)];
    for pane in panes {
        let Some(line) = agent_line(app, workspace, pane, "", width) else {
            continue;
        };
        lines.push(line);
        let subagents = app
            .subagents
            .get(&workspace_key(workspace.session.as_deref(), &pane.id))
            .map_or(&[][..], Vec::as_slice);
        let shown = if subagents.len() <= spare {
            subagents.len()
        } else {
            spare.saturating_sub(1)
        };
        for subagent in &subagents[..shown] {
            lines.push(subagent_line(app, subagent, width));
        }
        spare -= shown;
        if shown < subagents.len() && spare > 0 {
            lines.push(Line::styled(
                format!("  └ +{} more", subagents.len() - shown),
                dim,
            ));
            spare -= 1;
        }
    }
    lines
}

/// An agent's state icon, `prefix` and its name and title, and the time since
/// its state began.
fn agent_line(
    app: &App,
    workspace: &WorkspaceNode,
    pane: &PaneNode,
    prefix: &str,
    width: usize,
) -> Option<Line<'static>> {
    let agent = pane.agent.as_ref()?;
    let since = app
        .activity
        .pane(workspace.session.as_deref(), &pane.id)
        .filter(|recorded| AgentState::parse(&recorded.status) == agent.state)
        .map(|recorded| recorded.since);
    let elapsed = since
        .map(|since| glance::age(app.now, since))
        .unwrap_or_default();
    let title = match &pane.title {
        Some(title) => format!("{prefix}{} · {title}", agent.name),
        None => format!("{prefix}{}", agent.name),
    };
    let title = truncate_terminal(&title, width.saturating_sub(elapsed.len() + 4));
    let icon = glance::status_icon(agent.state, app.config.glance.icons, app.spinner_tick);
    Some(justified(
        vec![
            Span::styled(
                icon,
                Style::default().fg(agent_state_color(&app.theme, agent.state)),
            ),
            Span::raw(" "),
            Span::styled(title, Style::default().fg(app.theme.text)),
        ],
        vec![Span::styled(
            elapsed,
            Style::default().fg(app.theme.subtext0),
        )],
        width,
    ))
}

/// The Sessions view's detail pane: the session's name, directory and state,
/// then its Workspaces by Warmth group, numbered for `1`–`9`, then its agents.
/// Returns the largest useful scroll offset, which `Shift-J` and `Shift-K` move.
fn draw_session_detail(f: &mut Frame, app: &App, area: Rect) -> usize {
    let Some(state) = app
        .selected_entry()
        .and_then(|entry| app.session_state(entry))
    else {
        return 0;
    };
    let area = Rect {
        x: area.x + 2,
        y: area.y + 1.min(area.height),
        width: area.width.saturating_sub(3),
        height: area.height.saturating_sub(2),
    };
    let width = area.width as usize;
    let dim = Style::default().fg(app.theme.subtext0);
    let mut lines = vec![
        Line::styled(
            truncate_terminal(Session::label(state.session.name.as_deref()), width),
            Style::default()
                .fg(app.theme.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(truncate_end(&home_relative(&state.session.dir), width), dim),
    ];
    if state.topology.is_none() {
        lines.push(Line::styled("stopped · enter starts it", dim));
        f.render_widget(Paragraph::new(lines), area);
        return 0;
    }
    let rows = app.session_workspaces(state);
    let count = match rows.len() {
        1 => "1 workspace".to_string(),
        count => format!("{count} workspaces"),
    };
    let focus = if state.current {
        Some("this session".to_string())
    } else {
        app.session_summary(state)
            .last_focused
            .map(|label| format!("last focused {label}"))
    };
    let status = ["running".to_string(), count]
        .into_iter()
        .chain(focus)
        .collect::<Vec<_>>()
        .join(" · ");
    lines.push(Line::styled(truncate_end(&status, width), dim));

    if !rows.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("WORKSPACES", dim));
    }
    let name_width = rows
        .iter()
        .map(|(row, _)| Span::raw(&row.name).width())
        .max()
        .unwrap_or(0)
        .min(width / 2);
    let branch_width = rows
        .iter()
        .map(|(row, _)| Span::raw(&row.branch).width())
        .max()
        .unwrap_or(0)
        .min(width.saturating_sub(name_width + 16));
    let mut group = None;
    for (index, (row, _)) in rows.iter().enumerate() {
        if group != Some(row.group) {
            if group.is_some() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::styled(
                row.group.label().to_uppercase(),
                Style::default()
                    .fg(group_color(&app.theme, row.group))
                    .add_modifier(Modifier::BOLD),
            ));
            group = Some(row.group);
        }
        let number = if index < 9 {
            (index + 1).to_string()
        } else {
            " ".into()
        };
        lines.push(justified(
            vec![
                Span::styled(
                    number,
                    Style::default()
                        .fg(app.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    fit_cell(
                        glance::status_icon(row.state, app.config.glance.icons, app.spinner_tick),
                        1,
                    ),
                    Style::default().fg(agent_state_color(&app.theme, row.state)),
                ),
                Span::raw("  "),
                Span::styled(
                    fit_cell(&row.name, name_width),
                    Style::default()
                        .fg(repository_color(&row.repository, &[], &app.theme))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(fit_cell(&row.branch, branch_width), dim),
            ],
            vec![Span::styled(glance::signal_text(&row.signal, app.now), dim)],
            width,
        ));
    }

    let agents: Vec<Line<'static>> = rows
        .iter()
        .flat_map(|(_, workspace)| {
            workspace
                .tabs
                .iter()
                .flat_map(|tab| &tab.panes)
                .filter_map(|pane| {
                    agent_line(
                        app,
                        workspace,
                        pane,
                        &format!("{} · ", workspace.label),
                        width,
                    )
                })
        })
        .collect();
    if !agents.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("AGENTS", dim));
        lines.extend(agents);
    }
    let max_offset = lines.len().saturating_sub(area.height as usize);
    let offset = app.detail_scroll.offset.min(max_offset);
    f.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), area);
    max_offset
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
        .style(Style::default().bg(app.theme.popup_background()));
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
        Source::Workspace | Source::Session => theme.green,
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
        app::{pin_key, Workspaces, OTHER_SESSION_NOTICE},
        config::Config,
        model::EntryAction,
        sessions::{Session, SessionState},
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

        let backend = TestBackend::new(70, 11);
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
        let backend = TestBackend::new(60, 11);
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
        let backend = TestBackend::new(60, 11);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();

        assert!(buffer_text(&terminal).contains("Helm"));
    }

    #[test]
    fn picker_paints_list_card_and_detail_panel_on_the_popup_background() {
        let app = detail_app(vec![claude_pane()], 0, ActivityState::default());
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let bg = |x: u16, y: u16| buffer[(x, y)].bg;
        let theme = &app.theme;

        for (x, y) in [(0, 0), (119, 1), (0, 29), (2, 1), (117, 28)] {
            assert_eq!(bg(x, y), theme.popup_background(), "({x}, {y})");
        }
        assert_eq!(bg(0, 5), theme.popup_background());
        assert_eq!(bg(1, 5), theme.popup_background());
        assert_eq!(bg(2, 5), theme.bar_background());
        assert_eq!(bg(3, 5), theme.bar_background());

        let gap = (3..119)
            .find(|&x| bg(x, 5) != theme.bar_background())
            .unwrap();
        assert_eq!(bg(gap, 5), theme.popup_background());
        assert_eq!(bg(gap + 1, 5), theme.popup_background());
        assert_eq!(bg(gap + 2, 5), theme.chrome_background());
        assert_eq!(bg(118, 5), theme.chrome_background());
        assert_eq!(bg(119, 5), theme.popup_background());
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

        handle_key(&mut app, key(KeyCode::Char('?')));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('q'))),
            Action::Continue
        );
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
                status: None,
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
                session: None,
                sessions: Vec::new(),
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
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        (content_lines(&lines), hits)
    }

    /// The rendered lines inside the content inset, so layout assertions
    /// read the same at any padding.
    fn content_lines(lines: &[String]) -> Vec<String> {
        let inner = lines.get(1..lines.len().saturating_sub(1)).unwrap_or(&[]);
        inner
            .iter()
            .map(|line| {
                let chars: Vec<char> = line.chars().collect();
                let end = chars.len().saturating_sub(1).max(2.min(chars.len()));
                chars[2.min(chars.len())..end].iter().collect()
            })
            .collect()
    }

    #[test]
    fn small_popups_get_a_size_notice_instead_of_the_layout() {
        assert!(too_small(Rect::new(0, 0, 119, 40)));
        assert!(too_small(Rect::new(0, 0, 160, 31)));
        assert!(!too_small(Rect::new(0, 0, 120, 32)));

        let app = App::new(Config::default(), Theme::terminal());
        let mut terminal = Terminal::new(TestBackend::new(98, 24)).unwrap();
        terminal.draw(|f| draw_too_small(f, &app)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("Helm needs at least 120 × 32, this popup is 98 × 24"));
    }

    #[test]
    fn content_sits_inside_an_even_inset() {
        let app = glance_app(
            NOW,
            None,
            vec![commit_workspace("w1", "dots", NOW - 60)],
            ActivityState::default(),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let text = buffer_text(&terminal);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].trim().is_empty());
        assert!(lines[lines.len() - 1].trim().is_empty());
        assert!(lines[1].starts_with("   Helm"));
        assert!(lines[2].starts_with("   "));
        assert!(lines[2].contains("default"));
        assert!(lines[lines.len() - 2].starts_with("   a–g"));
        assert!(lines[lines.len() - 2].ends_with("q quit  "));
    }

    #[test]
    fn the_legend_sits_on_the_last_row_of_the_list_in_either_icon_style() {
        for (icons, legend) in [
            (
                IconStyle::Nerd,
                "⠋  working  \u{F009E}  waits  \u{F05E0}  done  \u{F0766}  idle",
            ),
            (IconStyle::Unicode, "◐  working  ●  waits  ✔  done  ○  idle"),
        ] {
            let mut app = glance_app(
                NOW,
                None,
                vec![commit_workspace("w1", "dots", NOW - 60)],
                ActivityState::default(),
            );
            app.config.glance.icons = icons;
            let (lines, hits) = render_app(&app, 160, 40);
            let row = line_index(&lines, legend);
            assert_eq!(row, line_index(&lines, "q quit") - 3);
            let hot = line_index(&lines, "HOT");
            assert_eq!(
                column_of(&lines[row], legend),
                column_of(&lines[hot], "HOT")
            );
            assert_eq!(hits.area.bottom() as usize, row + 1);
        }
    }

    #[test]
    fn a_narrow_legend_drops_entries_from_the_right() {
        let app = App::new(Config::default(), Theme::terminal());
        let mut terminal = Terminal::new(TestBackend::new(30, 1)).unwrap();
        terminal
            .draw(|f| {
                draw_legend(f, &app, f.area());
            })
            .unwrap();
        assert_eq!(
            buffer_text(&terminal).trim_end(),
            " ⠋  working  \u{F009E}  waits  \u{F05E0}  done"
        );
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
        activity.record_agent_status(None, "p-helm", "working", NOW - 14 * 60);
        activity.record_agent_status(None, "p-skills", "blocked", NOW - 4 * 60);
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

        let (lines, _) = render_app(&app, 100, 24);
        let labels = ["NEEDS YOU", "HOT", "WARM", "COLD"];
        let headings = labels.map(|label| line_index(&lines, label));
        assert!(headings.windows(2).all(|pair| pair[0] < pair[1]));
        for (label, heading) in labels.iter().zip(headings) {
            assert_eq!(lines[heading].trim(), *label);
        }
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
        activity.record_agent_status(None, "p-helm", "working", NOW - 14 * 60);
        activity.record_agent_status(None, "p-skills", "blocked", NOW - 4 * 60);
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

        let (lines, _) = render_app(&app, 100, 22);
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
    fn the_state_icon_follows_the_label_and_the_previous_workspace_still_drives_l() {
        let mut app = glance_app(
            NOW,
            Some("w2"),
            vec![
                glance_workspace("w1", "helm", true, Some(NOW - 60), vec![]),
                glance_workspace("w2", "skills", false, Some(NOW - 120), vec![]),
                glance_workspace("w3", "koyo", false, Some(NOW - 180), vec![]),
            ],
            ActivityState::default(),
        );

        let (lines, _) = render_app(&app, 100, 22);
        for name in ["helm", "skills", "koyo"] {
            let line = &lines[line_index(&lines, &format!("{name}-branch"))];
            assert!(!line.contains('●') && !line.contains('◌'), "{line}");
        }
        let helm = &lines[line_index(&lines, "helm-branch")];
        assert_eq!(column_of(helm, "helm"), column_of(helm, " a ") + 7);
        assert!(!lines
            .iter()
            .any(|line| line.contains(" here") || line.contains('◌')));

        assert_eq!(handle_key(&mut app, key(KeyCode::Char('l'))), Action::Open);
        assert_eq!(selected_title(&app), "skills");
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
        let heading = line_index(&lines, "DIRECTORIES");
        assert_eq!(lines[heading].trim(), "DIRECTORIES");
        assert!(lines[heading + 1].contains("projects-dir"));
        assert!(!lines.iter().any(|line| line.contains("helm")));
    }

    #[test]
    fn search_keeps_the_group_layout_and_drops_non_matching_rows() {
        let mut activity = ActivityState::default();
        activity.record_agent_status(None, "p-skills", "blocked", NOW - 4 * 60);
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
        assert_eq!(lines[line_index(&lines, "NEEDS YOU")].trim(), "NEEDS YOU");
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
        let row = line_index(&lines, "lazyjira") as u16 + 1;
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
        activity.record_agent_status(None, "p1", "working", NOW - 60);
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
        activity.record_focus(None, "w1", NOW - 6 * 60);
        activity.record_agent_status(None, "p-helm", "working", NOW - 14 * 60);
        activity.record_agent_status(None, "p-skills", "blocked", NOW - 4 * 60);
        activity.record_agent_status(None, "p-writing", "done", NOW - 12 * 60);
        activity.record_agent_status(None, "p-mdn", "working", NOW - 14 * 60);
        activity.record_focus(None, "w5", NOW - 38 * 60);
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

        let pinned = line_index(&lines, "PINNED");
        let header = &lines[pinned - 2];
        assert!(lines[pinned - 1].chars().take(80).all(|ch| ch == ' '));
        let fonts = &lines[line_index(&lines, "fonts")];
        assert_eq!(
            column_of(header, "ST"),
            column_of(&lines[pinned], "PINNED") + 4
        );
        assert_eq!(
            column_of(header, "KEY"),
            column_of(&lines[pinned], "PINNED")
        );
        assert_eq!(column_of(header, "WORKSPACE"), column_of(fonts, "fonts"));
        assert_eq!(
            column_of(header, "BRANCH"),
            column_of(fonts, "fonts-branch")
        );
        assert_eq!(column_of(header, "AGE"), column_of(fonts, "9d"));
        assert_eq!(
            column_of(header, "PROGRESS"),
            column_of(header, "AGE") + "AGE".len() + 2
        );
    }

    #[test]
    fn the_git_cell_shows_counts_or_clean_and_cuts_from_the_right() {
        let app = App::new(Config::default(), Theme::terminal());
        let text = |parts: &[(String, Color)]| {
            parts
                .iter()
                .map(|(text, _)| text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let busy = GitStatus {
            staged: 2,
            modified: 1,
            untracked: 3,
            ahead_behind: Some((4, 5)),
        };
        let parts = git_parts(&app, Some(busy));
        assert_eq!(text(&parts), "+2 !1 ?3 ↑4 ↓5");
        let colours: Vec<Color> = parts.iter().map(|(_, colour)| *colour).collect();
        assert_eq!(
            colours,
            [
                app.theme.green,
                app.theme.yellow,
                app.theme.subtext0,
                app.theme.accent,
                app.theme.red
            ]
        );
        assert_eq!(git_width(&parts), 14);

        let ahead = GitStatus {
            ahead_behind: Some((1, 0)),
            ..GitStatus::default()
        };
        assert_eq!(text(&git_parts(&app, Some(ahead))), "↑1");
        let clean = git_parts(&app, Some(GitStatus::default()));
        assert_eq!(clean, [("✓".to_string(), app.theme.green)]);
        assert!(git_parts(&app, None).is_empty());

        let cell = |width| {
            git_cell(&parts, width)
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        assert_eq!(cell(16), "+2 !1 ?3 ↑4 ↓5  ");
        assert_eq!(cell(12), "+2 !1 ?3 ↑4 ");
        assert_eq!(cell(10), "+2 !1 ?3 …");
    }

    #[test]
    fn the_git_column_sits_between_branch_and_age_at_160_columns() {
        let mut busy = commit_workspace("w1", "helm", NOW - 60);
        busy.git.as_mut().unwrap().status = Some(GitStatus {
            staged: 2,
            modified: 1,
            untracked: 0,
            ahead_behind: Some((3, 0)),
        });
        let mut clean = commit_workspace("w2", "dots", NOW - 120);
        clean.git.as_mut().unwrap().status = Some(GitStatus::default());
        let unknown = commit_workspace("w3", "koyo", NOW - 180);
        let app = glance_app(
            NOW,
            None,
            vec![busy, clean, unknown],
            ActivityState::default(),
        );

        let (lines, _) = render_app(&app, 160, 40);
        let header = &lines[line_index(&lines, "WORKSPACE")];
        let helm = &lines[line_index(&lines, "helm-branch  +2")];
        let dots = &lines[line_index(&lines, "dots-branch  ✓")];
        let koyo = &lines[line_index(&lines, "koyo-branch")];
        assert!(column_of(header, "BRANCH") < column_of(header, "GIT"));
        assert!(column_of(header, "GIT") < column_of(header, "AGE"));
        assert_eq!(column_of(header, "GIT"), column_of(helm, "+2 !1 ↑3"));
        assert_eq!(column_of(header, "GIT"), column_of(dots, "✓"));
        assert_eq!(column_of(header, "AGE"), column_of(helm, "1m"));
        assert_eq!(column_of(header, "AGE"), column_of(koyo, "3m"));
        assert_eq!(column_of(header, "BRANCH"), column_of(koyo, "koyo-branch"));
    }

    #[test]
    fn the_column_header_stays_on_top_while_the_list_scrolls() {
        let workspaces = (0..30)
            .map(|n| {
                commit_workspace(
                    &format!("w{n}"),
                    &format!("repo{n:02}"),
                    NOW - 86_400 * (n + 1),
                )
            })
            .collect();
        let mut app = glance_app(NOW, None, workspaces, ActivityState::default());
        app.selected = app.filtered.len() - 1;

        let (lines, hits) = render_app(&app, 160, 32);
        let header = line_index(&lines, "WORKSPACE");
        assert!(lines[header].contains("BRANCH"));
        assert!(!lines.iter().any(|line| line.contains("COLD")));
        assert!(!lines.iter().any(|line| line.contains("repo00")));
        assert!(line_index(&lines, "repo29") > header);
        let first_hit = hits.rows.first().unwrap().0.start;
        assert_eq!(first_hit, hits.area.y);
        assert!(hits.rows.iter().all(|(rows, _)| rows.start >= hits.area.y));
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
    fn c_twice_sends_the_workspace_to_cold_and_any_other_key_cancels() {
        let dir = std::env::temp_dir().join(format!("helm-cold-key-test-{}", std::process::id()));
        let _lock = crate::herdr::test_env_lock();
        let previous = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR");
        std::env::set_var("HERDR_PLUGIN_CONFIG_DIR", &dir);

        let mut app = glance_app(
            NOW,
            None,
            vec![
                commit_workspace("w1", "alpha", NOW - 60),
                commit_workspace("w2", "beta", NOW - 120),
            ],
            ActivityState::default(),
        );
        handle_key(&mut app, key(KeyCode::Char('c')));
        assert!(app.cold_armed);
        let (lines, _) = render_app(&app, 160, 40);
        assert!(lines
            .last()
            .unwrap()
            .contains("c again to send to cold alpha"));
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert!(!app.cold_armed);
        assert!(app.cold_entries.is_empty());

        handle_key(&mut app, key(KeyCode::Char('k')));
        let alpha = app.selected_entry().unwrap().clone();
        handle_key(&mut app, key(KeyCode::Char('c')));
        handle_key(&mut app, key(KeyCode::Char('c')));
        assert!(app.is_cold(&alpha));

        app.selected = app
            .filtered
            .iter()
            .position(|index| app.entries[*index].title == alpha.title)
            .unwrap();
        handle_key(&mut app, key(KeyCode::Char('c')));
        assert!(!app.is_cold(&alpha));
        assert!(app.cold_entries.is_empty());

        app.cold_entries
            .insert(crate::app::pin_key(&alpha), NOW - 120);
        app.rebuild_glance();
        assert!(!app.is_cold(&alpha));
        app.selected = app
            .filtered
            .iter()
            .position(|index| app.entries[*index].title == alpha.title)
            .unwrap();
        handle_key(&mut app, key(KeyCode::Char('c')));
        assert!(app.cold_armed);
        handle_key(&mut app, key(KeyCode::Char('c')));
        assert!(app.is_cold(&alpha));
        assert!(app.cold_entries[&crate::app::pin_key(&alpha)] > NOW);

        let _ = std::fs::remove_dir_all(&dir);
        match previous {
            Some(value) => std::env::set_var("HERDR_PLUGIN_CONFIG_DIR", value),
            None => std::env::remove_var("HERDR_PLUGIN_CONFIG_DIR"),
        }
    }

    #[test]
    fn x_twice_closes_the_workspace_and_any_other_key_cancels() {
        let mut app = glance_app(
            NOW,
            None,
            vec![
                commit_workspace("w1", "alpha", NOW - 60),
                commit_workspace("w2", "beta", NOW - 120),
            ],
            ActivityState::default(),
        );
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('x'))),
            Action::Continue
        );
        assert!(app.close_armed);
        let (lines, _) = render_app(&app, 160, 40);
        assert!(lines.last().unwrap().contains("x again to close alpha"));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('x'))),
            Action::CloseWorkspace
        );

        handle_key(&mut app, key(KeyCode::Char('x')));
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert!(!app.close_armed);
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('x'))),
            Action::Continue
        );
    }

    #[test]
    fn q_closes_in_normal_mode_and_types_while_searching() {
        let mut app = glance_app(
            NOW,
            None,
            vec![commit_workspace("w1", "alpha", NOW - 60)],
            ActivityState::default(),
        );
        handle_key(&mut app, key(KeyCode::Char('/')));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('q'))),
            Action::Continue
        );
        assert_eq!(app.query, "q");
        handle_key(&mut app, key(KeyCode::Esc));
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn label_selects_the_labelled_row_and_is_ignored_while_searching() {
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

        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('s'))),
            Action::Continue
        );
        assert_eq!(selected_title(&app), "beta");
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('s'))), Action::Open);
        assert_eq!(selected_title(&app), "beta");

        handle_key(&mut app, key(KeyCode::Char('a')));
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('a'))),
            Action::Continue
        );

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
        activity.record_agent_status(None, "p-need", "blocked", NOW - 60);
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
        activity.record_agent_status(None, "p-first", "blocked", NOW - 60);
        activity.record_agent_status(None, "p-second", "done", NOW - 120);
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

        assert_eq!(handle_key(&mut app, key(KeyCode::Char('l'))), Action::Open);
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
        let label_at = |name: &str| lines[line_index(&lines, name)].chars().nth(3).unwrap();
        assert_eq!(label_at("golf"), 'a');
        assert_eq!(label_at("alpha"), 's');
        assert_eq!(label_at("bravo"), 'd');
        assert_eq!(label_at("charlie"), 'f');
        assert_eq!(label_at("delta"), 'g');
        assert_eq!(label_at("echo"), ' ');
        assert_eq!(label_at("foxtrot"), ' ');

        handle_key(&mut app, key(KeyCode::Char('/')));
        let (lines, _) = render_app(&app, 100, 40);
        assert_eq!(lines[line_index(&lines, "golf")].chars().nth(3), Some(' '));
    }

    #[test]
    fn header_and_footer_render_at_160_and_100_columns() {
        let mut activity = ActivityState::default();
        activity.record_agent_status(None, "p-skills", "blocked", NOW - 4 * 60);
        activity.record_agent_status(None, "p-writing", "done", NOW - 12 * 60);
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
        assert!(wide[0].trim_start().starts_with("Helm"));
        assert!(wide[0].contains("2 need you"));
        assert!(wide[0].contains("1 hot"));
        assert!(wide[0].contains("1 cold"));
        assert!(!wide[0].contains("warm"));
        assert!(wide[1].contains("default  / type to search"));
        assert!(wide[2].contains("^W workspaces"));
        assert!(wide[2].contains("^R roots"));
        assert!(wide[2].contains("^Z zoxide"));
        let footer = wide.last().unwrap();
        assert!(footer.contains("a–g row"));
        assert!(footer.contains("enter open"));
        assert!(footer.contains("1–9 tab"));
        assert!(footer.contains("l last"));
        assert!(footer.contains("x close"));
        assert!(
            footer.find("a–g row") < footer.find("1–9 tab")
                && footer.find("1–9 tab") < footer.find("enter open")
                && footer.find("enter open") < footer.find("l last")
        );
        assert!(footer.contains("] [ group"));
        assert!(footer.contains("! needs you"));
        assert!(footer.contains("/ search"));
        assert!(!footer.contains("^B pin"));
        assert!(footer.trim_end().ends_with("? keys   q quit"));

        let (narrow, _) = render_app(&app, 100, 40);
        let footer = narrow.last().unwrap();
        assert!(footer.contains("a–g row"));
        assert!(footer.trim_end().ends_with("? keys   q quit"));
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
        assert!(footer.contains("a–g row"));
        assert!(!footer.contains("/ search"));
        assert!(!footer.contains("^B pin"));
        assert!(footer.trim_end().ends_with("? keys   q quit"));
    }

    #[test]
    fn needs_you_finds_and_counts_pinned_rows_that_wait_on_the_user() {
        let mut activity = ActivityState::default();
        activity.record_agent_status(None, "p-pinned", "blocked", NOW - 60);
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
        let (lines, _) = render_app(&app, 124, 34);
        let text = lines.join("\n");
        for section in [" Move", " Open", " Find", " Sources", " Act"] {
            assert!(text.contains(section), "{section}");
        }
        assert!(text.contains("J K"));
        assert!(text.contains("a–g"));
        assert!(text.contains("send to cold (c twice) / warm up"));
        assert!(text.contains("this sheet"));
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
    fn detail_pane_hides_when_the_content_is_narrower_than_110_columns() {
        let app = detail_app(vec![claude_pane()], 0, ActivityState::default());
        let (narrow, _) = render_app(&app, 112, 40);
        assert!(!narrow.iter().any(|line| line.contains("AGENTS")));
        let (wide, _) = render_app(&app, 113, 40);
        assert!(wide.iter().any(|line| line.contains("AGENTS")));
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
            app.subagents.insert(
                format!("<default>::{id}"),
                vec![subagent("Port the tests", 3, 6, NOW - 192)],
            );
        }
        let (lines, _) = render_app(&app, 100, 20);
        assert!(lines[line_index(&lines, "helm")].contains("▰▰▰▰▱▱▱▱"));
        assert!(!lines[line_index(&lines, "skills")].contains('▰'));
    }

    #[test]
    fn list_progress_column_shows_summed_subagent_progress() {
        let mut app = detail_app(vec![claude_pane()], 0, ActivityState::default());
        app.subagents.insert(
            "<default>::p1".into(),
            vec![subagent("Port the tests", 3, 6, NOW - 192)],
        );
        let (lines, _) = render_app(&app, 100, 40);
        assert!(lines[line_index(&lines, "helm.herdr")].contains("▰▰▰▰▱▱▱▱"));
    }

    #[test]
    fn glance_detail_renders_agents_changes_and_commits_at_180_by_45() {
        let mut agent = claude_pane();
        agent.title = Some("Port the glance detail pane".into());
        let mut activity = ActivityState::default();
        activity.record_agent_status(None, "p1", "working", NOW - 14 * 60);
        let mut app = detail_app(vec![agent], 0, activity);
        app.subagents.insert(
            "<default>::p1".into(),
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

    fn agent_workspace(id: &str, label: &str, state: AgentState) -> WorkspaceNode {
        let pane_id = format!("p-{label}");
        glance_workspace(
            id,
            label,
            false,
            None,
            vec![pane(&pane_id, "/repos", Some(("claude", state)))],
        )
    }

    fn refresh_with(app: &mut App, workspaces: Vec<WorkspaceNode>, activity: ActivityState) {
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology { workspaces },
                activity,
                previous: None,
                subagents: Default::default(),
                session: None,
                sessions: Vec::new(),
            },
            NOW,
        );
    }

    fn row_titles(app: &App) -> Vec<String> {
        app.filtered
            .iter()
            .map(|index| app.entries[*index].title.clone())
            .collect()
    }

    fn working_since(sinces: &[(&str, i64)]) -> ActivityState {
        let mut activity = ActivityState::default();
        for (label, since) in sinces {
            activity.record_agent_status(None, &format!("p-{label}"), "working", *since);
        }
        activity
    }

    #[test]
    fn a_refresh_inside_one_group_keeps_rows_and_labels_in_place() {
        let workspaces = || {
            vec![
                agent_workspace("w1", "alpha", AgentState::Working),
                agent_workspace("w2", "bravo", AgentState::Working),
                agent_workspace("w3", "charlie", AgentState::Working),
            ]
        };
        let mut app = glance_app(
            NOW,
            None,
            workspaces(),
            working_since(&[
                ("alpha", NOW - 60),
                ("bravo", NOW - 120),
                ("charlie", NOW - 180),
            ]),
        );
        assert_eq!(row_titles(&app), ["alpha", "bravo", "charlie"]);
        app.selected = 1;

        refresh_with(
            &mut app,
            workspaces(),
            working_since(&[
                ("alpha", NOW - 60),
                ("bravo", NOW - 120),
                ("charlie", NOW - 5),
            ]),
        );

        assert_eq!(row_titles(&app), ["alpha", "bravo", "charlie"]);
        assert_eq!(selected_title(&app), "bravo");
        let labelled: Vec<String> = app
            .labelled_positions()
            .into_iter()
            .map(|position| app.entries[app.filtered[position]].title.clone())
            .collect();
        assert_eq!(labelled, ["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn a_refresh_moves_only_rows_that_change_group_and_appends_new_rows() {
        let mut app = glance_app(
            NOW,
            None,
            vec![
                agent_workspace("w1", "alpha", AgentState::Working),
                agent_workspace("w2", "bravo", AgentState::Working),
                agent_workspace("w3", "charlie", AgentState::Working),
            ],
            working_since(&[
                ("alpha", NOW - 60),
                ("bravo", NOW - 120),
                ("charlie", NOW - 180),
            ]),
        );
        app.selected = 2;

        refresh_with(
            &mut app,
            vec![
                agent_workspace("w2", "bravo", AgentState::Working),
                agent_workspace("w3", "charlie", AgentState::Blocked),
                agent_workspace("w4", "delta", AgentState::Working),
            ],
            working_since(&[("bravo", NOW - 120), ("delta", NOW - 1)]),
        );

        assert_eq!(row_titles(&app), ["charlie", "bravo", "delta"]);
        assert_eq!(selected_title(&app), "charlie");
    }

    fn many_subagents(count: u32) -> Vec<Subagent> {
        (1..=count)
            .map(|index| subagent(&format!("Subagent step {index}"), 1, 4, NOW - 60))
            .collect()
    }

    fn crowded_detail_app() -> App {
        let mut second = pane("p2", "/repos/helm.herdr", Some(("codex", AgentState::Idle)));
        second.title = Some("Second agent".into());
        let mut app = detail_app(vec![claude_pane(), second], 0, ActivityState::default());
        app.subagents
            .insert("<default>::p1".into(), many_subagents(8));
        app.cache_git(WORKSPACE_KEY.into(), dirty_git(), NOW);
        app
    }

    #[test]
    fn detail_keeps_every_agent_and_both_sections_at_120_by_27() {
        let (lines, _) = render_app(&crowded_detail_app(), 120, 27);
        let second = line_index(&lines, "codex · Second agent");
        assert!(line_index(&lines, "AGENTS") < second);
        assert!(second < line_index(&lines, "CHANGES · 3 files"));
        assert!(line_index(&lines, "CHANGES · 3 files") < line_index(&lines, "RECENT COMMITS"));
        assert!(lines[line_index(&lines, "CHANGES · 3 files") - 1]
            .trim()
            .is_empty());
        assert!(lines.iter().any(|line| line.contains("3f1a9c0")));
    }

    #[test]
    fn detail_keeps_every_agent_and_both_sections_at_120_by_31() {
        let (lines, _) = render_app(&crowded_detail_app(), 120, 31);
        let second = line_index(&lines, "codex · Second agent");
        assert!(line_index(&lines, "Subagent step 1") < second);
        assert!(second < line_index(&lines, "CHANGES · 3 files"));
        assert!(line_index(&lines, "CHANGES · 3 files") < line_index(&lines, "RECENT COMMITS"));
        assert!(lines.iter().any(|line| line.contains("3f1a9c0")));
    }

    #[test]
    fn tab_chips_on_their_own_line_never_crowd_out_the_workspace_name() {
        let mut app = detail_app(vec![claude_pane()], 0, ActivityState::default());
        let workspace = &mut app.topology.workspaces[0];
        workspace.label = "helm.herdr-glance-view".into();
        for index in 0..6 {
            workspace.tabs.push(TabNode {
                id: format!("w1:extra{index}"),
                label: format!("long tab label {index}"),
                focused: false,
                panes: Vec::new(),
            });
        }
        app.rebuild_glance();
        let (lines, _) = render_app(&app, 120, 30);
        let chips = line_index(&lines, "1  Agents");
        let name = line_index(&lines, "helm.herdr-glance-view");
        assert!(chips < name, "chips {chips}, name {name}");
        let detail_column = |line: &str| line.chars().skip(64).collect::<String>();
        assert!(detail_column(&lines[chips + 1]).trim().is_empty());
        assert_eq!(name, chips + 2);
    }

    #[test]
    fn the_selected_row_keeps_its_label_background() {
        let app = glance_app(
            NOW,
            None,
            vec![agent_workspace("w1", "api", AgentState::Idle)],
            ActivityState::default(),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut hits = ListHits::default();
        terminal.draw(|f| hits = draw(f, &app).0).unwrap();
        let (rows, _) = hits.rows[0].clone();
        let buffer = terminal.backend().buffer();
        let label = (hits.area.x..hits.area.right())
            .find(|&x| {
                buffer[(x, rows.start)].symbol() == app.config.glance.labels()[0].to_string()
            })
            .unwrap();
        assert_eq!(buffer[(label, rows.start)].bg, app.theme.surface0);
    }

    fn other_session_app() -> App {
        let mut waiting = agent_workspace("w1", "api", AgentState::Blocked);
        waiting.session = Some("work".into());
        waiting.focused = true;
        let mut idle = agent_workspace("w2", "notes", AgentState::Idle);
        idle.session = Some("work".into());
        let mut app = glance_app(
            NOW,
            None,
            vec![
                agent_workspace("w1", "skills", AgentState::Blocked),
                commit_workspace("w2", "dots", NOW - 60),
                waiting,
                idle,
            ],
            ActivityState::default(),
        );
        app.selected = app
            .filtered
            .iter()
            .position(|index| app.entries[*index].title == "api")
            .unwrap();
        app
    }

    #[test]
    fn other_session_rows_show_their_tag_and_the_header_counts_them() {
        let app = other_session_app();
        let (lines, _) = render_app(&app, 160, 40);

        assert!(lines[0].contains("2 need you · 1 elsewhere"));
        let api = &lines[line_index(&lines, "api @work")];
        let skills = &lines[line_index(&lines, "skills")];
        assert!(!api.contains('●'));
        assert_eq!(
            column_of(api, "api-branch"),
            column_of(skills, "skills-branch")
        );
        assert!(!lines.iter().any(|line| line.contains("notes")));
    }

    #[test]
    fn the_session_tag_takes_the_mauve_colour() {
        let app = other_session_app();
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = (0..buffer.area.height)
            .find_map(|y| {
                let line: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect();
                line.find("@work")
                    .map(|byte| (line[..byte].chars().count() as u16, y))
            })
            .unwrap();
        assert_eq!(buffer[(x, y)].fg, app.theme.mauve);
    }

    #[test]
    fn a_tight_name_column_clips_the_name_and_keeps_the_tag() {
        let cells = GlanceCells {
            icon: "",
            icon_color: Color::Reset,
            name: "helm.herdr".into(),
            tag: Some("@work".into()),
            tag_color: Color::Reset,
            name_color: Color::Reset,
            branch: String::new(),
            git: None,
            signal: String::new(),
        };
        let text = |width| {
            name_cell(&cells, width, Style::default(), Style::default())
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        assert_eq!(text(18), "helm.herdr @work  ");
        assert_eq!(text(10), "hel… @work");
        assert_eq!(text(7), "helm.h…");
    }

    #[test]
    fn opening_an_other_session_row_keeps_helm_open_with_a_notice() {
        let mut app = other_session_app();
        let label = app
            .labelled_positions()
            .iter()
            .position(|&position| position == app.selected)
            .map(|slot| app.config.glance.labels()[slot])
            .unwrap();

        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), Action::Continue);
        assert_eq!(app.notice.as_deref(), Some(OTHER_SESSION_NOTICE));
        let (lines, _) = render_app(&app, 160, 40);
        assert!(lines
            .last()
            .unwrap()
            .contains("Start Herdr with helm-herdr shell to switch sessions"));

        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('j'))),
            Action::Continue
        );
        assert_eq!(app.notice.as_deref(), None);
        app.selected = app.labelled_positions()[app
            .config
            .glance
            .labels()
            .iter()
            .position(|ch| *ch == label)
            .unwrap()];

        handle_key(&mut app, key(KeyCode::Char(label)));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char(label))),
            Action::Continue
        );
        assert_eq!(app.notice.as_deref(), Some(OTHER_SESSION_NOTICE));

        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('1'))),
            Action::Continue
        );
        assert_eq!(app.notice.as_deref(), Some(OTHER_SESSION_NOTICE));
    }

    #[test]
    fn close_cold_and_pin_leave_other_session_rows_alone() {
        let mut app = other_session_app();
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('x'))),
            Action::Continue
        );
        assert!(!app.close_armed);
        handle_key(&mut app, key(KeyCode::Char('c')));
        assert!(!app.cold_armed);
        assert_eq!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
            ),
            Action::Continue
        );
        assert_eq!(selected_title(&app), "api");
    }

    fn in_session(mut workspace: WorkspaceNode, session: &str) -> WorkspaceNode {
        workspace.session = Some(session.into());
        workspace
    }

    fn session_state(
        name: Option<&str>,
        current: bool,
        workspaces: Option<Vec<WorkspaceNode>>,
    ) -> SessionState {
        let root = PathBuf::from("/h/.config/herdr");
        SessionState {
            session: Session {
                name: name.map(str::to_string),
                dir: name.map_or(root.clone(), |name| root.join("sessions").join(name)),
            },
            current,
            topology: workspaces.map(|workspaces| OpenTopology { workspaces }),
        }
    }

    /// Helm runs in `work`, with `b` and `c` running and `default` stopped.
    fn sessions_app() -> App {
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(sessions_load(NOW - 600), NOW);
        app.set_filter(Some(Source::Session));
        app.apply_filter();
        app
    }

    fn sessions_load(b_focused_at: i64) -> Workspaces {
        let mut helm = agent_workspace("w1", "helm", AgentState::Blocked);
        helm.focused = true;
        let local = vec![
            in_session(helm, "work"),
            in_session(commit_workspace("w2", "dots", NOW - 2 * 3_600), "work"),
            in_session(commit_workspace("w3", "old", NOW - 3 * 86_400), "work"),
        ];
        let api = in_session(agent_workspace("w1", "api", AgentState::Done), "b");
        let web = in_session(agent_workspace("w2", "web", AgentState::Working), "b");
        let docs = in_session(commit_workspace("w1", "docs", NOW - 10 * 86_400), "c");
        let mut activity = ActivityState::default();
        activity.record_focus(Some("b"), "w1", b_focused_at);
        activity.record_focus(Some("c"), "w1", NOW - 60);
        let mut topology = local.clone();
        topology.push(api.clone());
        Workspaces {
            topology: OpenTopology {
                workspaces: topology,
            },
            activity,
            previous: Some("w2".into()),
            subagents: Default::default(),
            session: Some("work".into()),
            sessions: vec![
                session_state(None, false, None),
                session_state(Some("b"), false, Some(vec![api, web])),
                session_state(Some("c"), false, Some(vec![docs])),
                session_state(Some("work"), true, Some(Vec::new())),
            ],
        }
    }

    fn visible_titles(app: &App) -> Vec<String> {
        app.filtered
            .iter()
            .map(|index| app.entries[*index].title.clone())
            .collect()
    }

    #[test]
    fn sessions_view_lists_the_current_session_then_running_by_activity_then_stopped() {
        let app = sessions_app();
        assert_eq!(visible_titles(&app), ["work", "c", "b", "default"]);
        assert_eq!(app.selected, 0);

        let summary = |name: &str| {
            let state = app
                .sessions
                .iter()
                .find(|state| state.session.name.as_deref() == Some(name))
                .unwrap();
            app.session_summary(state)
        };
        let b = summary("b");
        assert_eq!(b.workspaces, 2);
        assert_eq!(b.state, Some(AgentState::Done));
        assert_eq!(b.needs_you, Some((AgentState::Done, 1)));
        assert_eq!(b.last_active, Some(NOW - 600));
        assert_eq!(b.last_focused.as_deref(), Some("api"));
        let work = summary("work");
        assert_eq!(work.workspaces, 3);
        assert_eq!(work.needs_you, Some((AgentState::Blocked, 1)));
        let c = summary("c");
        assert_eq!((c.state, c.needs_you), (None, None));
    }

    #[test]
    fn sessions_view_renders_groups_tags_counts_and_start_at_160_columns() {
        let app = sessions_app();
        let (lines, _) = render_app(&app, 160, 40);

        let strip = &lines[line_index(&lines, "^S sessions")];
        assert!(column_of(strip, "^W") < column_of(strip, "^S"));
        assert!(column_of(strip, "^S") < column_of(strip, "^Z"));
        assert!(line_index(&lines, "RUNNING") < line_index(&lines, "STOPPED"));
        assert_eq!(
            line_index(&lines, "⠋  working  "),
            line_index(&lines, "q quit") - 3
        );
        let work = &lines[line_index(&lines, "work 1 need you")];
        let b = &lines[line_index(&lines, "b 1 need you")];
        let c = &lines[line_index(&lines, " c ")];
        let default = &lines[line_index(&lines, "default")];
        assert!(!work.contains('●') && work.contains("3 ws"));
        assert!(b.contains("2 ws") && b.contains("10m"));
        assert!(c.contains("1 ws") && c.contains("1m"));
        assert!(default.contains("start") && !default.contains(" ws"));
        assert_eq!(column_of(work, "3 ws"), column_of(b, "2 ws"));
        assert_eq!(column_of(b, "2 ws"), column_of(default, "start"));
        let header = &lines[line_index(&lines, "RUNNING") - 2];
        assert!(!header.contains("ST ") && !header.contains("PROGRESS"));
        assert!(!header.contains("GIT"));
        assert_eq!(column_of(header, "KEY"), column_of(work, " a "));
        assert_eq!(
            column_of(header, "SESSION"),
            column_of(work, "work 1 need you")
        );
        assert_eq!(column_of(header, "WORKSPACES"), column_of(work, "3 ws"));
        assert_eq!(column_of(header, "AGE"), column_of(b, "10m"));

        let footer = lines.last().unwrap();
        for hint in [
            "a–g row",
            "1–9 switch to workspace",
            "enter switch",
            "l last",
            "/ search",
            "? keys",
            "q quit",
        ] {
            assert!(footer.contains(hint), "{hint} missing from {footer}");
        }
        assert!(!footer.contains("close") && !footer.contains("group"));
    }

    #[test]
    fn the_need_you_tag_takes_its_state_colour() {
        let app = sessions_app();
        let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = (0..buffer.area.height)
            .find_map(|y| {
                let line: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect();
                line.find("1 need you")
                    .filter(|_| line.contains(" b "))
                    .map(|byte| (line[..byte].chars().count() as u16, y))
            })
            .unwrap();
        assert_eq!(buffer[(x, y)].fg, app.theme.teal);
    }

    #[test]
    fn session_detail_groups_and_numbers_workspaces_like_the_digit_keys() {
        let app = sessions_app();
        let (lines, _) = render_app(&app, 160, 40);
        assert!(lines
            .iter()
            .any(|line| line.contains("/h/.config/herdr/sessions/work")));
        assert!(lines
            .iter()
            .any(|line| line.contains("running · 3 workspaces · this session")));
        let needs_you = line_index(&lines, "NEEDS YOU");
        let warm = line_index(&lines, "WARM");
        let cold = line_index(&lines, "COLD");
        assert!(needs_you < warm && warm < cold);
        let detail_column = column_of(&lines[warm], "WARM");
        let gap: String = lines[warm - 1].chars().skip(detail_column).collect();
        assert!(gap.trim().is_empty(), "no blank line before WARM");
        for (number, label) in [(1, "helm"), (2, "dots"), (3, "old")] {
            let line = &lines[line_index(&lines, &format!("{label}-branch"))];
            let detail: String = line.chars().skip(detail_column).collect();
            assert!(detail.starts_with(&format!("{number} ")), "{line}");
            assert!(detail.contains(label), "{number} {label}: {line}");
            assert!(!detail.contains('●') && !detail.contains('◌'), "{line}");
            let Some(EntryAction::FocusWorkspace { id, .. }) = app.session_action(Some(number))
            else {
                panic!("no workspace for {number}");
            };
            assert_eq!(id, format!("w{number}"));
        }
        assert!(lines.iter().any(|line| line.contains("AGENTS")));
        assert!(lines.iter().any(|line| line.contains("helm · claude")));
    }

    #[test]
    fn detail_of_other_and_stopped_sessions() {
        let mut app = sessions_app();
        app.selected = 2;
        let (lines, _) = render_app(&app, 160, 40);
        assert!(lines
            .iter()
            .any(|line| line.contains("running · 2 workspaces · last focused api")));
        let api = &lines[line_index(&lines, "api-branch")];
        assert!(!api.contains('●'));
        assert!(line_index(&lines, "NEEDS YOU") < line_index(&lines, "HOT"));

        app.selected = 3;
        let (lines, _) = render_app(&app, 160, 40);
        assert!(lines
            .iter()
            .any(|line| line.contains("stopped · enter starts it")));
        assert!(!lines.iter().any(|line| line
            .chars()
            .skip(80)
            .collect::<String>()
            .contains("WORKSPACES")));
    }

    #[test]
    fn ctrl_s_switches_to_the_sessions_view_and_back() {
        let mut app = sessions_app();
        app.set_filter(None);
        app.apply_filter();
        let ctrl = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        handle_key(&mut app, ctrl('s'));
        assert!(app.sessions_view());
        assert_eq!(visible_titles(&app)[0], "work");
        handle_key(&mut app, ctrl('w'));
        assert!(app.glance_view() && !app.sessions_view());
        assert!(keybindings(&app).iter().any(|binding| binding.command()
            == Command::Filter(Source::Session)
            && binding.key_label(&app) == "^S"
            && binding.label == "sessions"));
    }

    #[test]
    fn opening_another_session_keeps_helm_open_and_the_current_one_returns_to_the_glance() {
        let mut app = sessions_app();
        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), Action::Continue);
        assert!(app.glance_view());
        assert_eq!(app.notice.as_deref(), None);

        let mut app = sessions_app();
        handle_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), Action::Continue);
        assert_eq!(app.notice.as_deref(), Some(OTHER_SESSION_NOTICE));
        assert!(app.sessions_view());

        handle_key(&mut app, key(KeyCode::Char('d')));
        assert_eq!(visible_titles(&app)[app.selected], "b");
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('d'))),
            Action::Continue
        );
        assert_eq!(app.notice.as_deref(), Some(OTHER_SESSION_NOTICE));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('1'))),
            Action::Continue
        );
        assert_eq!(app.notice.as_deref(), Some(OTHER_SESSION_NOTICE));
        assert!(matches!(
            app.session_action(Some(1)),
            Some(EntryAction::SwitchSession { session: Some(session), workspace: Some(workspace), .. })
                if session == "b" && workspace == "w1"
        ));

        handle_key(&mut app, key(KeyCode::Char('f')));
        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), Action::Continue);
        assert_eq!(app.notice.as_deref(), Some(OTHER_SESSION_NOTICE));

        handle_key(&mut app, key(KeyCode::Char('a')));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('2'))),
            Action::OpenTab(2)
        );
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('9'))),
            Action::Continue
        );
    }

    #[test]
    fn close_cold_and_pin_leave_session_rows_alone_and_l_opens_the_previous_workspace() {
        let mut app = sessions_app();
        handle_key(&mut app, key(KeyCode::Char('x')));
        assert!(!app.close_armed);
        handle_key(&mut app, key(KeyCode::Char('c')));
        assert!(!app.cold_armed);
        let pinned = app.pinned_entries.clone();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.pinned_entries, pinned);

        assert_eq!(handle_key(&mut app, key(KeyCode::Char('l'))), Action::Open);
        assert!(app.glance_view());
        assert_eq!(selected_title(&app), "dots");
    }

    #[test]
    fn a_query_narrows_sessions_by_name() {
        let mut app = sessions_app();
        app.query = "b".into();
        app.apply_filter();
        assert_eq!(visible_titles(&app), ["b"]);
        app.query = "def".into();
        app.apply_filter();
        assert_eq!(visible_titles(&app), ["default"]);
        let (lines, _) = render_app(&app, 160, 40);
        assert!(lines.iter().any(|line| line.contains("STOPPED")));
        assert!(!lines.iter().any(|line| line.contains("RUNNING")));
    }

    #[test]
    fn other_sessions_get_git_from_a_read_kept_for_its_ttl() {
        let mut app = sessions_app();
        assert!(app.session_git_target(NOW).is_none());
        app.selected = 2;
        let state = app.sessions[1].clone();
        let branch = |app: &App, label: &str| {
            app.session_workspaces(&state)
                .into_iter()
                .find(|(row, _)| row.name == label)
                .map(|(row, _)| row.branch)
                .unwrap()
        };
        assert_eq!(branch(&app, "api"), "api-branch");
        assert_eq!(branch(&app, "web"), "");

        let (label, topology) = app.session_git_target(NOW).unwrap();
        assert_eq!(label, "b");
        assert_eq!(topology.workspaces.len(), 2);
        app.session_git.insert(label, (topology, NOW));
        assert_eq!(branch(&app, "web"), "web-branch");
        assert!(app.session_git_target(NOW + 29).is_none());
        assert!(app.session_git_target(NOW + 30).is_some());
    }

    #[test]
    fn session_rows_keep_their_place_while_helm_is_open() {
        let mut app = sessions_app();
        app.apply_workspaces(sessions_load(NOW - 5), NOW);
        assert_eq!(visible_titles(&app), ["work", "c", "b", "default"]);

        let mut fresh = App::new(Config::default(), Theme::terminal());
        fresh.apply_workspaces(sessions_load(NOW - 5), NOW);
        fresh.set_filter(Some(Source::Session));
        fresh.apply_filter();
        assert_eq!(visible_titles(&fresh), ["work", "b", "c", "default"]);
    }

    #[test]
    fn digits_past_another_sessions_workspaces_do_nothing() {
        let mut app = sessions_app();
        for (row, digit) in [(2, '9'), (3, '1')] {
            app.selected = row;
            assert_eq!(
                handle_key(&mut app, key(KeyCode::Char(digit))),
                Action::Continue
            );
            assert_eq!(app.notice.as_deref(), None, "row {row}");
        }
    }

    #[test]
    fn the_key_sheet_lists_the_sessions_filter_at_the_smallest_popup() {
        let mut app = sessions_app();
        handle_key(&mut app, key(KeyCode::Char('?')));
        let (lines, _) = render_app(&app, 120, 32);
        let text = lines.join("\n");
        assert!(lines
            .iter()
            .any(|line| line.contains("^S") && line.contains("sessions")));
        assert!(text.contains("switch to numbered workspace"));
        assert!(text.contains("this sheet"));
        assert!(!text.contains("next / previous group"));
        assert!(!text.contains("next that needs you"));
    }

    #[test]
    fn an_unselected_other_session_row_shows_its_need_you_tag() {
        let local = vec![
            in_session(commit_workspace("w1", "helm.herdr", NOW - 60), "a"),
            in_session(commit_workspace("w2", "helm.herdr-wt", NOW - 60), "a"),
        ];
        let blocked = in_session(
            agent_workspace("w1", "helm.herdr", AgentState::Blocked),
            "b",
        );
        let mut topology = local;
        topology.push(blocked.clone());
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology {
                    workspaces: topology,
                },
                activity: ActivityState::default(),
                previous: None,
                subagents: Default::default(),
                session: Some("a".into()),
                sessions: vec![
                    session_state(None, false, None),
                    session_state(Some("a"), true, Some(Vec::new())),
                    session_state(Some("b"), false, Some(vec![blocked])),
                ],
            },
            NOW,
        );
        app.set_filter(Some(Source::Session));
        app.apply_filter();
        assert_eq!(app.selected, 0);
        let (lines, _) = render_app(&app, 124, 34);
        let b = &lines[line_index(&lines, " b ")];
        assert!(b.contains("b 1 need you"), "{b}");
        assert!(b.contains("1 ws"), "{b}");
    }

    #[test]
    fn shift_j_scrolls_a_long_session_detail() {
        let workspaces: Vec<WorkspaceNode> = (1..=30)
            .map(|n| {
                in_session(
                    commit_workspace(&format!("w{n}"), &format!("ws{n:02}"), NOW - n * 60),
                    "a",
                )
            })
            .collect();
        let mut app = App::new(Config::default(), Theme::terminal());
        app.apply_workspaces(
            Workspaces {
                topology: OpenTopology { workspaces },
                activity: ActivityState::default(),
                previous: None,
                subagents: Default::default(),
                session: Some("a".into()),
                sessions: vec![session_state(Some("a"), true, Some(Vec::new()))],
            },
            NOW,
        );
        app.set_filter(Some(Source::Session));
        app.apply_filter();
        let mut terminal = Terminal::new(TestBackend::new(160, 32)).unwrap();
        terminal
            .draw(|f| app.detail_max_offset = draw(f, &app).1)
            .unwrap();
        assert!(app.detail_max_offset > 0);
        assert!(!buffer_text(&terminal).contains("ws30"));

        for _ in 0..app.detail_max_offset {
            handle_key(&mut app, shift_key('J'));
        }
        terminal
            .draw(|f| {
                draw(f, &app);
            })
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("ws30"));
        assert!(!text.contains("this session"));
    }

    /// A launcher record for `session` naming this test process and a fake
    /// Herdr client, in a fresh launchers directory.
    #[cfg(unix)]
    fn live_launcher(app: &mut App, tag: &str) -> (PathBuf, crate::sessions::test_processes::Kid) {
        let dir = std::env::temp_dir().join(format!("helm-launcher-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let client = crate::sessions::test_processes::fake_client(&dir);
        crate::sessions::write_record(
            &dir,
            &crate::sessions::LauncherRecord {
                launcher_pid: std::process::id(),
                client_pid: client.id(),
                session: Session::label(app.session.as_deref()).to_string(),
            },
        )
        .unwrap();
        app.launchers_dir = Some(dir.clone());
        (dir, client)
    }

    #[cfg(unix)]
    fn switch_request(dir: &std::path::Path) -> serde_json::Value {
        let path = dir.join(format!("switch-{}.json", std::process::id()));
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn opening_an_other_session_row_switches_through_the_launcher() {
        use std::os::unix::process::ExitStatusExt;

        let mut app = other_session_app();
        let (dir, mut client) = live_launcher(&mut app, "glance");
        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), Action::Switched);
        assert_eq!(app.notice.as_deref(), None);
        assert_eq!(client.wait().unwrap().signal(), Some(15));
        assert_eq!(
            switch_request(&dir),
            serde_json::json!({"session": "work", "workspace_id": "w1", "tab_id": "w1:t1", "pane_id": "p-api"})
        );
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, _client) = live_launcher(&mut app, "tab");
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('1'))),
            Action::Switched
        );
        assert_eq!(
            switch_request(&dir),
            serde_json::json!({"session": "work", "workspace_id": "w1", "tab_id": "w1:t1"})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn sessions_view_switches_to_the_last_focused_or_numbered_workspace() {
        let mut app = sessions_app();
        let b = visible_titles(&app).iter().position(|t| t == "b").unwrap();
        let stopped = visible_titles(&app)
            .iter()
            .position(|t| t == "default")
            .unwrap();

        let (dir, _client) = live_launcher(&mut app, "enter");
        app.selected = b;
        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), Action::Switched);
        assert_eq!(
            switch_request(&dir),
            serde_json::json!({"session": "b", "workspace_id": "w1"})
        );
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, _client) = live_launcher(&mut app, "digit");
        app.selected = b;
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('2'))),
            Action::Switched
        );
        let numbered = app.session_workspaces(
            app.sessions
                .iter()
                .find(|state| state.session.name.as_deref() == Some("b"))
                .unwrap(),
        )[1]
        .1
        .id
        .clone();
        assert_eq!(switch_request(&dir)["workspace_id"], numbered.as_str());
        let _ = std::fs::remove_dir_all(&dir);

        let (dir, _client) = live_launcher(&mut app, "stopped");
        app.selected = stopped;
        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), Action::Switched);
        assert_eq!(
            switch_request(&dir),
            serde_json::json!({"session": "default"})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn l_targets_the_newest_focus_across_sessions_but_never_the_current_workspace() {
        let mut app = sessions_app();
        let elsewhere = |session: &str, id: &str| Some((Some(session.to_string()), id.to_string()));
        let target = |app: &App| app.previous_elsewhere();
        assert_eq!(target(&app), elsewhere("c", "w1"));

        app.activity.record_focus(Some("work"), "w1", NOW);
        assert_eq!(target(&app), elsewhere("c", "w1"));

        app.activity.record_focus(Some("work"), "w2", NOW - 30);
        assert_eq!(target(&app), None);

        app.activity.record_focus(Some("b"), "w2", NOW - 10);
        assert_eq!(target(&app), elsewhere("b", "w2"));
    }

    #[cfg(unix)]
    #[test]
    fn l_switches_sessions_only_with_a_live_launcher() {
        let mut app = sessions_app();
        app.set_filter(None);
        app.apply_filter();
        assert_eq!(handle_key(&mut app, key(KeyCode::Char('l'))), Action::Open);

        let (dir, _client) = live_launcher(&mut app, "last");
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char('l'))),
            Action::Switched
        );
        assert_eq!(
            switch_request(&dir),
            serde_json::json!({"session": "c", "workspace_id": "w1"})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_footer_names_the_session_enter_switches_to() {
        let mut app = other_session_app();
        let (lines, _) = render_app(&app, 160, 40);
        assert!(lines.last().unwrap().contains("enter switch to work"));

        app.selected = app
            .filtered
            .iter()
            .position(|index| app.entries[*index].title == "skills")
            .unwrap();
        let (lines, _) = render_app(&app, 160, 40);
        let footer = lines.last().unwrap();
        assert!(footer.contains("enter open") && !footer.contains("switch to"));
    }

    /// The label letter of the selected row.
    fn selected_label(app: &App) -> char {
        app.labelled_positions()
            .iter()
            .position(|&position| position == app.selected)
            .map(|slot| app.config.glance.labels()[slot])
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_row_label_twice_switches_and_closes_the_popup_in_the_glance_and_sessions_view() {
        let mut app = other_session_app();
        let (dir, _client) = live_launcher(&mut app, "label");
        let label = selected_label(&app);
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char(label))),
            Action::Continue
        );
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char(label))),
            Action::Switched
        );
        assert_eq!(
            switch_request(&dir),
            serde_json::json!({"session": "work", "workspace_id": "w1", "tab_id": "w1:t1", "pane_id": "p-api"})
        );
        let _ = std::fs::remove_dir_all(&dir);

        let mut app = sessions_app();
        app.selected = visible_titles(&app).iter().position(|t| t == "b").unwrap();
        let (dir, _client) = live_launcher(&mut app, "sessions-label");
        let label = selected_label(&app);
        handle_key(&mut app, key(KeyCode::Char(label)));
        assert_eq!(
            handle_key(&mut app, key(KeyCode::Char(label))),
            Action::Switched
        );
        assert_eq!(
            switch_request(&dir),
            serde_json::json!({"session": "b", "workspace_id": "w1"})
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_launcher_that_cannot_take_the_request_gets_its_own_notice() {
        use std::os::unix::fs::PermissionsExt;

        let mut app = other_session_app();
        let (dir, mut client) = live_launcher(&mut app, "failed");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), Action::Continue);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(app.notice.as_deref(), Some("Switching to work failed"));
        assert!(client.try_wait().unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_mouse_click_clears_the_notice() {
        let mut app = other_session_app();
        app.notice = Some(OTHER_SESSION_NOTICE.into());
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut app, click, &ListHits::default());
        assert_eq!(app.notice, None);
    }
}
