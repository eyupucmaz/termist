//! Rendering: header, cards, live pane and footer.
use crate::app::{App, Mode};
use crate::keys::{Action, Context, Keymap};
use crate::overlay_view;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use termist_core::{AgentStatus, Snapshot, cell_flags};

const CARD_W: u16 = 24;
const CARD_H: u16 = 4;

pub struct Areas {
    pub header: Rect,
    /// Everything between the header and the footer: cards and pane.
    pub body: Rect,
    pub cards: Rect,
    pub pane: Rect,
    pub pane_inner: Rect,
    pub footer: Rect,
    pub cards_per_row: usize,
    /// Rows of cards that fit.
    pub card_rows: usize,
    /// Not every card fits: the line above and the line below the cards count the
    /// hidden ones.
    pub scroll_lines: bool,
}

pub fn layout(area: Rect, session_count: usize) -> Areas {
    let header = Rect {
        height: area.height.min(1),
        ..area
    };
    let footer_h = area.height.saturating_sub(1).min(1);
    let footer = Rect {
        y: area.y + area.height - footer_h,
        height: footer_h,
        ..area
    };
    let body = Rect {
        y: area.y + header.height,
        height: area.height.saturating_sub(header.height + footer_h),
        ..area
    };
    let cards_per_row = (body.width / CARD_W).max(1) as usize;
    let card_rows = session_count.max(1).div_ceil(cards_per_row) as u16;
    let room = body.height / 2;
    let cards_h = (card_rows * CARD_H).min(room);
    let scroll_lines = card_rows * CARD_H > room;
    let (cards, visible_rows) = if scroll_lines {
        let rows = (cards_h.saturating_sub(2) / CARD_H).max(1);
        let cards = Rect {
            y: body.y + 1,
            height: (rows * CARD_H).min(cards_h.saturating_sub(1)),
            ..body
        };
        (cards, rows)
    } else {
        (
            Rect {
                height: cards_h,
                ..body
            },
            card_rows,
        )
    };
    let pane = Rect {
        y: body.y + cards_h,
        height: body.height - cards_h,
        ..body
    };
    let pane_inner = Rect {
        x: pane.x + 1,
        y: pane.y + 1,
        width: pane.width.saturating_sub(2),
        height: pane.height.saturating_sub(2),
    };
    Areas {
        header,
        body,
        cards,
        pane,
        pane_inner,
        footer,
        cards_per_row,
        card_rows: visible_rows as usize,
        scroll_lines,
    }
}

/// A status's glyph, its colour in `theme` and its word.
pub fn status_style(theme: &Theme, status: AgentStatus) -> (char, Color, &'static str) {
    let (glyph, word) = match status {
        AgentStatus::Fresh => ('●', "fresh"),
        AgentStatus::Running => ('●', "running"),
        AgentStatus::Unseen => ('✓', "done"),
        AgentStatus::Finished => ('●', "ready"),
        AgentStatus::NeedsFeedback => ('◆', "waiting"),
        AgentStatus::Exited { code: Some(0) } => ('●', "closed"),
        AgentStatus::Exited { .. } => ('✗', "exited"),
        AgentStatus::Disconnected => ('○', "disconnected"),
    };
    (glyph, theme.status(status), word)
}

pub fn draw(f: &mut Frame, app: &App, areas: &Areas) {
    let area = f.area();
    f.buffer_mut().set_style(area, app.theme.base);
    draw_header(f, app, areas.header);
    let sessions = app.project_sessions();
    if sessions.is_empty() {
        let key = |action| app.keymap.key(Context::Grid, action);
        let text = if !app.connected {
            "Connecting to the termist daemon…".to_string()
        } else if app.state.projects.is_empty() {
            "No project yet: run termist inside a project folder.".to_string()
        } else if app.project.is_none() {
            match key(Action::OpenProject) {
                Some(o) => format!("No project open · {o} opens one"),
                None => "No project open".to_string(),
            }
        } else if app.archive_view {
            match key(Action::ArchiveView) {
                Some(a) => format!("Nothing archived in this project.  {a}: back"),
                None => "Nothing archived in this project.  Esc: back".to_string(),
            }
        } else {
            let hints: Vec<String> = [Action::QuickPrompt, Action::NewSession, Action::NewShell]
                .into_iter()
                .filter_map(|a| Some(format!("{}: {}", key(a)?, a.hint())))
                .collect();
            format!("No sessions yet.  {}", hints.join("  ·  "))
        };
        f.render_widget(Paragraph::new(text).style(app.theme.dim), areas.body);
    } else {
        let per_row = areas.cards_per_row.max(1);
        let first = app.card_scroll;
        for (i, s) in sessions.iter().enumerate() {
            let row = i / per_row;
            if row < first || row >= first + areas.card_rows {
                continue;
            }
            let rect = Rect {
                x: areas.cards.x + (i % per_row) as u16 * CARD_W,
                y: areas.cards.y + (row - first) as u16 * CARD_H,
                width: CARD_W,
                height: CARD_H,
            };
            if rect.bottom() > areas.cards.bottom() || rect.right() > areas.cards.right() {
                continue;
            }
            draw_card(f, &app.theme, s, Some(s.id) == app.selected, rect);
        }
        if areas.scroll_lines {
            let above = first * per_row;
            let below = sessions
                .len()
                .saturating_sub((first + areas.card_rows) * per_row);
            let line = |y: u16| Rect {
                y,
                height: 1,
                ..areas.cards
            };
            for (n, arrow, rect) in [
                (above, '↑', line(areas.cards.y.saturating_sub(1))),
                (below, '↓', line(areas.cards.bottom())),
            ] {
                if n > 0 && rect.y < areas.pane.y {
                    f.render_widget(
                        Paragraph::new(format!("{arrow} {n} more")).style(app.theme.dim),
                        rect,
                    );
                }
            }
        }
        draw_pane(f, app, areas);
    }
    for (i, overlay) in app.overlays.iter().enumerate() {
        overlay_view::draw(f, app, overlay, areas.body, i + 1 == app.overlays.len());
    }
    draw_footer(f, app, areas.footer);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        " termist ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if app.archive_view {
        spans.push(Span::styled(
            "archive ",
            app.theme.archive.add_modifier(Modifier::BOLD),
        ));
    }
    for p in app.open_projects() {
        let style = if Some(p.id) == app.project {
            app.theme.tab_active
        } else {
            Style::default()
        };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!(" {} ", p.name), style));
        for status in [
            AgentStatus::NeedsFeedback,
            AgentStatus::Unseen,
            AgentStatus::Running,
        ] {
            let n = app
                .state
                .sessions
                .iter()
                .filter(|s| s.project == p.id && s.status == status && !s.archived)
                .count();
            if n > 0 {
                let (glyph, color, _) = status_style(&app.theme, status);
                spans.push(Span::styled(
                    format!("{glyph}{n}"),
                    Style::default().fg(color),
                ));
            }
        }
    }
    // Agents waiting in closed projects; the first thing to go when space is short.
    let waiting = app.waiting_in_closed_projects().len();
    if waiting > 0 {
        let (glyph, color, _) = status_style(&app.theme, AgentStatus::NeedsFeedback);
        let marker = [
            Span::styled("  closed ", app.theme.dim),
            Span::styled(format!("{glyph}{waiting}"), Style::default().fg(color)),
        ];
        let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
        if width(&spans) + width(&marker) <= area.width as usize {
            spans.extend(marker);
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_card(
    f: &mut Frame,
    theme: &Theme,
    s: &termist_core::SessionInfo,
    selected: bool,
    rect: Rect,
) {
    let (glyph, color, word) = status_style(theme, s.status);
    let border = if selected {
        theme.accent.add_modifier(Modifier::BOLD)
    } else {
        theme.border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if selected {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(border);
    let name = s.display_name();
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(color)),
            Span::raw(name.to_string()),
        ]),
        Line::from(Span::styled(
            format!("{} · {word}", s.kind.label()),
            theme.dim,
        )),
    ];
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

fn draw_pane(f: &mut Frame, app: &App, areas: &Areas) {
    let Some(info) = app.selected_info() else {
        return;
    };
    let focused = matches!(app.mode, Mode::Focus | Mode::FocusPrefix);
    let title = format!(
        " {} — {}{} ",
        info.display_name(),
        info.kind.label(),
        if focused { " · typing" } else { "" }
    );
    let border = if focused {
        app.theme.focus
    } else {
        app.theme.border
    };
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border),
        areas.pane,
    );
    if let Some(screen) = app.screens.get(&info.id) {
        render_screen(f.buffer_mut(), areas.pane_inner, screen, &app.theme);
        let c = screen.cursor;
        // An overlay on top has the keys; a text box places its own cursor.
        if focused
            && app.overlays.is_empty()
            && screen.modes.show_cursor
            && c.col < areas.pane_inner.width
            && c.row < areas.pane_inner.height
        {
            f.set_cursor_position((areas.pane_inner.x + c.col, areas.pane_inner.y + c.row));
        }
    } else if info.status == AgentStatus::Disconnected {
        f.render_widget(
            Paragraph::new("Not running. Enter resumes this session.").style(app.theme.dim),
            areas.pane_inner,
        );
    }
}

fn render_screen(buf: &mut Buffer, area: Rect, screen: &Snapshot, theme: &Theme) {
    for (r, line) in screen.lines.iter().enumerate().take(area.height as usize) {
        for (c, cell) in line.iter().enumerate().take(area.width as usize) {
            if cell.flags & cell_flags::WIDE_SPACER != 0 {
                continue;
            }
            let mut style = Style::default()
                .fg(theme.pane_color(cell.fg, true))
                .bg(theme.pane_color(cell.bg, false));
            for (flag, modifier) in [
                (cell_flags::BOLD, Modifier::BOLD),
                (cell_flags::ITALIC, Modifier::ITALIC),
                (cell_flags::UNDERLINE, Modifier::UNDERLINED),
                (cell_flags::INVERSE, Modifier::REVERSED),
                (cell_flags::DIM, Modifier::DIM),
                (cell_flags::HIDDEN, Modifier::HIDDEN),
                (cell_flags::STRIKEOUT, Modifier::CROSSED_OUT),
            ] {
                if cell.flags & flag != 0 {
                    style = style.add_modifier(modifier);
                }
            }
            buf[(area.x + c as u16, area.y + r as u16)]
                .set_char(cell.ch)
                .set_style(style);
        }
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let shows_message = app.mode == Mode::Grid || !app.overlays.is_empty();
    let (text, style) = match (&app.message, app.mode) {
        (Some(m), _) if shows_message => (m.clone(), t.error),
        _ if !app.overlays.is_empty() => (
            app.overlays
                .last()
                .map(overlay_view::hint)
                .unwrap_or_default()
                .to_string(),
            t.focus,
        ),
        (_, Mode::ConfirmQuit) => (
            "Leave termist? Sessions keep running in the daemon.  y / Enter: quit · any key: stay"
                .into(),
            t.warn,
        ),
        (_, Mode::ConfirmKill(id)) => {
            let name = app
                .state
                .sessions
                .iter()
                .find(|s| s.id == id)
                .map_or("this session", |s| s.display_name());
            (
                format!("Kill {name}? It stops the process.  y / Enter: kill · any key: cancel"),
                t.warn,
            )
        }
        (_, Mode::ConfirmClose(id)) => {
            let name = app
                .state
                .projects
                .iter()
                .find(|p| p.id == id)
                .map_or("this project", |p| p.name.as_str());
            (
                format!(
                    "Close {name}? Its sessions keep running.  y / Enter: close · any key: cancel"
                ),
                t.warn,
            )
        }
        (_, Mode::ConfirmArchive(id)) => {
            let session = app.state.sessions.iter().find(|s| s.id == id);
            let name = session.map_or("this session", |s| s.display_name());
            let text = if session.is_some_and(|s| s.status.is_live()) {
                format!("Stop and archive {name}? y/N")
            } else {
                format!("Archive {name}? y/N")
            };
            (text, t.warn)
        }
        (_, Mode::Grid) if app.archive_view => (archive_hint(&app.keymap), t.dim),
        (_, Mode::Grid) => (grid_hint(&app.keymap), t.dim),
        (_, Mode::Focus) => (focus_hint(&app.keymap), t.dim),
        (_, Mode::FocusPrefix) => (prefix_hint(&app.keymap), t.focus),
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

/// `key hint` for each action that has a key, joined with ` · `.
fn hints(keymap: &Keymap, context: Context, actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .filter_map(|a| Some(format!("{} {}", keymap.key(context, *a)?, a.hint())))
        .collect()
}

/// The four move keys as one word when they are single characters (`hjkl`), else
/// joined with slashes; `None` unless all four are bound.
fn move_keys(keymap: &Keymap, context: Context) -> Option<String> {
    let keys =
        [Action::Left, Action::Down, Action::Up, Action::Right].map(|a| keymap.key(context, a));
    let keys: Vec<String> = keys.into_iter().collect::<Option<_>>()?;
    Some(if keys.iter().all(|k| k.chars().count() == 1) {
        keys.concat()
    } else {
        keys.join("/")
    })
}

fn grid_hint(keymap: &Keymap) -> String {
    use Action::*;
    hints(
        keymap,
        Context::Grid,
        &[
            Help,
            QuickPrompt,
            FollowUp,
            Palette,
            NewSession,
            NewShell,
            Focus,
            NextAttention,
            OpenProject,
            CloseTab,
            Rename,
            Archive,
            ArchiveView,
            Kill,
            Quit,
        ],
    )
    .join(" · ")
}

fn archive_hint(keymap: &Keymap) -> String {
    let key = |a| keymap.key(Context::Grid, a);
    let mut parts = vec![
        "archive".to_string(),
        "Enter restore and resume".to_string(),
    ];
    parts.extend(move_keys(keymap, Context::Grid).map(|m| format!("{m} move")));
    parts.extend(key(Action::Kill).map(|k| format!("{k} delete")));
    parts.push(match key(Action::ArchiveView) {
        Some(a) => format!("{a}/Esc back"),
        None => "Esc back".to_string(),
    });
    parts.extend(key(Action::Quit).map(|q| format!("{q} quit")));
    parts.join(" · ")
}

fn focus_hint(keymap: &Keymap) -> String {
    let prefix = keymap.prefix;
    let mut parts = vec!["typing into the session".to_string()];
    parts.extend(
        hints(
            keymap,
            Context::Focus,
            &[Action::Grid, Action::NextAttention],
        )
        .into_iter()
        .map(|h| format!("{prefix} {h}")),
    );
    parts.push("C-q grid".to_string());
    parts.join(" · ")
}

fn prefix_hint(keymap: &Keymap) -> String {
    let key = |a| keymap.key(Context::Focus, a);
    let mut parts = hints(keymap, Context::Focus, &[Action::Grid]);
    if let (Some(next), Some(prev)) = (key(Action::NextAttention), key(Action::PrevAttention)) {
        parts.push(format!("{next} {prev} next/prev●"));
    }
    parts.extend(move_keys(keymap, Context::Focus).map(|m| format!("{m} move")));
    parts.push(format!("{} literal", keymap.prefix));
    format!("{} …  {}", keymap.prefix, parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use termist_core::{
        ProjectId, ProjectInfo, ServerEvent, SessionId, SessionInfo, SessionKind, Snapshot,
        StateSnapshot, screen::diff,
    };

    fn render(app: &mut App, w: u16, h: u16) -> Terminal<TestBackend> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let areas = layout(Rect::new(0, 0, w, h), app.project_sessions().len());
        app.set_card_window(areas.cards_per_row, areas.card_rows);
        app.pane_resized(areas.pane_inner.width, areas.pane_inner.height);
        t.draw(|f| draw(f, app, &areas)).unwrap();
        t
    }

    fn fixture() -> App {
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "orbit-api".into(),
            path: "/x".into(),
            open: true,
        };
        let mk = |name: &str, kind: SessionKind, status| SessionInfo {
            id: SessionId::new(),
            project: p.id,
            kind,
            name: name.into(),
            status,
            agent_session_id: None,
            title: None,
            last_activity_ms: 0,
            model: None,
            effort: None,
            user_named: false,
            archived: false,
        };
        let waiting = mk(
            "claude-1",
            SessionKind::Agent {
                harness: termist_core::Harness::Claude,
            },
            AgentStatus::NeedsFeedback,
        );
        let done = mk("shell-2", SessionKind::Shell, AgentStatus::Unseen);
        let mut app = App::new();
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![p],
            sessions: vec![waiting.clone(), done],
            ..StateSnapshot::default()
        }));
        let mut snap = Snapshot::blank(10, 2);
        for (i, ch) in "hello".chars().enumerate() {
            snap.lines[0][i].ch = ch;
        }
        app.on_event(ServerEvent::Screen {
            session: waiting.id,
            update: diff(None, &snap).unwrap(),
        });
        app
    }

    #[test]
    fn grid_with_cards_and_pane() {
        let mut app = fixture();
        insta::assert_snapshot!(render(&mut app, 60, 16).backend());
    }

    #[test]
    fn empty_project_explains_what_to_do() {
        let mut app = App::new();
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![ProjectInfo {
                id: ProjectId::new(),
                name: "web".into(),
                path: "/w".into(),
                open: true,
            }],
            ..StateSnapshot::default()
        }));
        insta::assert_snapshot!(render(&mut app, 60, 10).backend());
    }

    #[test]
    fn status_glyphs_follow_the_prd() {
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::NeedsFeedback).0,
            '◆'
        );
        assert_eq!(status_style(&Theme::terminal(), AgentStatus::Unseen).0, '✓');
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Disconnected).0,
            '○'
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Exited { code: Some(1) }).0,
            '✗'
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Running).2,
            "running"
        );
    }

    // TestBackend's Display is text-only, so no snapshot would catch a swapped or
    // wrong colour. Pin the full (glyph, Color, word) tuple for every status.
    #[test]
    fn status_style_matches_the_global_table() {
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Fresh),
            ('●', Color::DarkGray, "fresh")
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Running),
            ('●', Color::Yellow, "running")
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Unseen),
            ('✓', Color::Blue, "done")
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Finished),
            ('●', Color::Green, "ready")
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::NeedsFeedback),
            ('◆', Color::Red, "waiting")
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Exited { code: Some(1) }),
            ('✗', Color::Magenta, "exited")
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Exited { code: Some(0) }),
            ('●', Color::DarkGray, "closed")
        );
        assert_eq!(
            status_style(&Theme::terminal(), AgentStatus::Disconnected),
            ('○', Color::Gray, "disconnected")
        );
    }

    fn row(t: &Terminal<TestBackend>, y: u16) -> String {
        let buf = t.backend().buffer();
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn before_the_first_state_it_says_it_is_connecting() {
        let mut app = App::new();
        let t = render(&mut app, 60, 10);
        assert_eq!(row(&t, 1), "Connecting to the termist daemon…");
        app.on_event(ServerEvent::State(StateSnapshot::default()));
        let t = render(&mut app, 60, 10);
        assert_eq!(
            row(&t, 1),
            "No project yet: run termist inside a project folder."
        );
    }

    #[test]
    fn confirm_kill_names_the_session_in_yellow() {
        let mut app = fixture();
        app.on_key(ratatui::crossterm::event::KeyEvent::from(
            ratatui::crossterm::event::KeyCode::Char('d'),
        ));
        let t = render(&mut app, 80, 16);
        let buf = t.backend().buffer();
        let footer: String = (0..80).map(|x| buf[(x, 15)].symbol()).collect();
        assert_eq!(
            footer.trim_end(),
            "Kill claude-1? It stops the process.  y / Enter: kill · any key: cancel"
        );
        assert_eq!(buf[(0, 15)].fg, Color::Yellow);
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for (w, h) in [(20, 5), (1, 1), (0, 0), (200, 3)] {
            let mut app = fixture();
            let mut t = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
            let areas = layout(Rect::new(0, 0, w, h), 2);
            t.draw(|f| draw(f, &app, &areas)).unwrap();
            app.pane_resized(areas.pane_inner.width, areas.pane_inner.height);
        }
    }

    #[test]
    fn a_disconnected_card_explains_how_to_resume_it() {
        let mut app = fixture();
        let id = app.selected.unwrap();
        let mut info = app.selected_info().unwrap().clone();
        info.status = AgentStatus::Disconnected;
        app.screens.remove(&id);
        app.on_event(ServerEvent::SessionUpdated(info));
        let t = render(&mut app, 60, 16);
        let text: String = t
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Enter resumes this session"));
    }

    #[test]
    fn the_picker_marks_missing_clis() {
        let mut app = fixture();
        app.on_event(ServerEvent::Harnesses(vec![
            termist_core::HarnessInfo {
                harness: termist_core::Harness::Claude,
                available: true,
            },
            termist_core::HarnessInfo {
                harness: termist_core::Harness::Codex,
                available: false,
            },
            termist_core::HarnessInfo {
                harness: termist_core::Harness::OpenCode,
                available: true,
            },
        ]));
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('n'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        let t = render(&mut app, 60, 16);
        let text: String = t
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("codex"));
        assert!(text.contains("not installed"));
        assert!(text.contains("opencode"));
    }

    fn key(code: ratatui::crossterm::event::KeyCode) -> ratatui::crossterm::event::KeyEvent {
        ratatui::crossterm::event::KeyEvent::from(code)
    }

    fn ctrl(c: char) -> ratatui::crossterm::event::KeyEvent {
        ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char(c),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        )
    }

    fn screen_text(t: &Terminal<TestBackend>) -> String {
        t.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn quick_prompt_with_its_launch_line() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.state.last_launch = Some(termist_core::LaunchOptions {
            harness: termist_core::Harness::Claude,
            model: Some("opus".into()),
            effort: Some("high".into()),
        });
        app.on_key(key(K::Char('p')));
        for c in "fix the login redirect".chars() {
            app.on_key(key(K::Char(c)));
        }
        insta::assert_snapshot!(render(&mut app, 60, 16).backend());
    }

    #[test]
    fn the_model_picker_offers_the_efforts_of_the_cli() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Char('p')));
        app.on_key(ctrl('o'));
        let text = screen_text(&render(&mut app, 80, 20));
        assert!(text.contains("CLI default"));
        assert!(text.contains("type a model…"));
        assert!(text.contains("xhigh"));
        app.on_key(key(K::Esc));
        app.on_key(key(K::Tab));
        app.on_key(key(K::Char('3')));
        app.on_key(ctrl('o'));
        let text = screen_text(&render(&mut app, 80, 20));
        assert!(text.contains("model · opencode"));
        assert!(!text.contains("effort"), "OpenCode has no effort flag");
    }

    #[test]
    fn a_name_the_user_gave_wins_over_the_agents_title() {
        let mut app = fixture();
        let mut info = app.selected_info().unwrap().clone();
        info.title = Some("Auto Title".into());
        app.on_event(ServerEvent::SessionUpdated(info.clone()));
        assert!(screen_text(&render(&mut app, 60, 16)).contains("Auto Title"));
        info.name = "mine".into();
        info.user_named = true;
        app.on_event(ServerEvent::SessionUpdated(info));
        let text = screen_text(&render(&mut app, 60, 16));
        assert!(text.contains("◆ mine"));
        assert!(!text.contains("Auto Title"));
    }

    // A list on top of the focused pane has no text cursor: the pane's must not show
    // through it. A text box on top shows its own.
    #[test]
    fn the_pane_cursor_hides_under_a_list_overlay() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Enter));
        assert!(render(&mut app, 60, 16).backend().cursor_visible());
        app.on_key(ctrl('a'));
        app.on_key(key(K::Char('/')));
        assert!(!render(&mut app, 60, 16).backend().cursor_visible());
        app.on_key(key(K::Esc));
        app.on_key(ctrl('a'));
        app.on_key(key(K::Char('p')));
        let t = render(&mut app, 60, 16);
        assert!(
            t.backend().cursor_visible(),
            "the quick prompt's own cursor"
        );
    }

    #[test]
    fn palette_over_the_grid() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Char('/')));
        insta::assert_snapshot!(render(&mut app, 70, 16).backend());
    }

    #[test]
    fn with_every_project_closed_the_grid_says_how_to_open_one() {
        let mut app = fixture();
        let mut state = app.state.clone();
        state.projects[0].open = false;
        app.on_event(ServerEvent::State(state));
        let t = render(&mut app, 60, 10);
        assert_eq!(row(&t, 0), " termist   closed ◆1", "its agent still waits");
        assert_eq!(row(&t, 1), "No project open · o opens one");
    }

    #[test]
    fn open_project_browser() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Char('o')));
        app.listed(
            std::path::Path::new("/"),
            Ok(crate::browse::Listing {
                entries: vec![
                    crate::browse::DirEntry {
                        name: "notes".into(),
                        path: "/notes".into(),
                        canonical: "/notes".into(),
                        git: false,
                    },
                    crate::browse::DirEntry {
                        name: "orbit-web".into(),
                        path: "/orbit-web".into(),
                        canonical: "/orbit-web".into(),
                        git: true,
                    },
                ],
                truncated: false,
            }),
        );
        insta::assert_snapshot!(render(&mut app, 70, 16).backend());
    }

    #[test]
    fn cards_that_do_not_fit_are_counted_above_and_below() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        let project = app.state.projects[0].id;
        let mut state = app.state.clone();
        for i in 3..=7 {
            let mut s = state.sessions[1].clone();
            s.id = SessionId::new();
            s.project = project;
            s.name = format!("shell-{i}");
            state.sessions.push(s);
        }
        app.on_event(ServerEvent::State(state));
        render(&mut app, 60, 16);
        for _ in 0..2 {
            app.on_key(key(K::Char('j')));
        }
        insta::assert_snapshot!(render(&mut app, 60, 16).backend());
    }

    #[test]
    fn the_archive_view() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        let mut info = app.state.sessions[1].clone();
        info.archived = true;
        info.status = AgentStatus::Exited { code: None };
        app.on_event(ServerEvent::SessionUpdated(info));
        let t = render(&mut app, 60, 16);
        assert_eq!(
            row(&t, 0),
            " termist   orbit-api ◆1",
            "archived cards are not counted"
        );
        app.on_key(key(K::Char('A')));
        insta::assert_snapshot!(render(&mut app, 60, 16).backend());
    }

    /// The fixture plus a closed project "notes" with two agents waiting, one running
    /// and one archived while waiting.
    fn with_a_closed_project() -> App {
        let mut app = fixture();
        let mut state = app.state.clone();
        let notes = ProjectInfo {
            id: ProjectId::new(),
            name: "notes".into(),
            path: "/notes".into(),
            open: false,
        };
        for (status, archived) in [
            (AgentStatus::NeedsFeedback, false),
            (AgentStatus::NeedsFeedback, false),
            (AgentStatus::Running, false),
            (AgentStatus::NeedsFeedback, true),
        ] {
            let mut s = state.sessions[0].clone();
            s.id = SessionId::new();
            s.project = notes.id;
            s.status = status;
            s.archived = archived;
            state.sessions.push(s);
        }
        state.projects.push(notes);
        app.on_event(ServerEvent::State(state));
        app
    }

    #[test]
    fn waiting_agents_of_closed_projects_are_counted_at_the_end_of_the_tab_bar() {
        let mut app = with_a_closed_project();
        let t = render(&mut app, 60, 16);
        assert_eq!(row(&t, 0), " termist   orbit-api ◆1✓1  closed ◆2");
        let buf = t.backend().buffer();
        assert_eq!(buf[(27, 0)].fg, Color::DarkGray, "the word is dimmed");
        assert_eq!(buf[(34, 0)].fg, Color::Red, "the diamond is red");
        insta::assert_snapshot!(t.backend());
    }

    #[test]
    fn a_narrow_tab_bar_drops_the_closed_marker_before_any_tab() {
        let mut app = with_a_closed_project();
        let t = render(&mut app, 30, 16);
        assert_eq!(row(&t, 0), " termist   orbit-api ◆1✓1");
        let mut app = fixture();
        let t = render(&mut app, 60, 16);
        assert_eq!(row(&t, 0), " termist   orbit-api ◆1✓1", "none waiting");
    }

    #[test]
    fn archiving_asks_first_and_says_whether_it_stops_something() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Char('a')));
        let t = render(&mut app, 60, 16);
        assert_eq!(row(&t, 15), "Stop and archive claude-1? y/N");
    }

    fn with_theme(mut app: App, id: &str) -> App {
        app.theme = Theme::named(id, termist_core::config::ColorDepth::TrueColor);
        app
    }

    /// A painting theme leaves no cell on the terminal's own background: not the
    /// grid, not the pane, not an overlay box.
    #[test]
    fn a_painting_theme_paints_every_cell() {
        use ratatui::crossterm::event::KeyCode as K;
        for id in ["uskudar", "moda"] {
            let mut app = with_theme(fixture(), id);
            let bg = app.theme.base.bg.unwrap();
            for open_palette in [false, true] {
                if open_palette {
                    app.on_key(key(K::Char('/')));
                }
                let t = render(&mut app, 70, 16);
                let buf = t.backend().buffer();
                for y in 0..16 {
                    for x in 0..70 {
                        let cell = &buf[(x, y)];
                        assert_ne!(cell.bg, Color::Reset, "{id} ({x},{y}) {:?}", cell.symbol());
                    }
                }
                assert_eq!(
                    buf[(69, 7)].bg,
                    bg,
                    "{id}: empty space is the theme's ground"
                );
            }
        }
    }

    #[test]
    fn the_terminal_theme_paints_nothing() {
        let mut app = fixture();
        let t = render(&mut app, 60, 16);
        assert_eq!(t.backend().buffer()[(59, 7)].bg, Color::Reset);
    }

    /// Pane cells: the agent's default colours and ANSI 0-15 come from the theme,
    /// 24-bit colours are drawn as written, and inverse video swaps the theme's pair.
    #[test]
    fn the_pane_draws_agent_colours_through_the_theme() {
        let mut app = with_theme(fixture(), "moda");
        let id = app.selected.unwrap();
        let mut snap = Snapshot::blank(10, 2);
        snap.lines[0][0].ch = 'r';
        snap.lines[0][0].fg = termist_core::Color::Indexed(1);
        snap.lines[0][1].ch = 'x';
        snap.lines[0][1].fg = termist_core::Color::Rgb(1, 2, 3);
        snap.lines[0][2].ch = 'i';
        snap.lines[0][2].flags = cell_flags::INVERSE;
        let before = app.screens[&id].clone();
        app.on_event(ServerEvent::Screen {
            session: id,
            update: diff(Some(&before), &snap).unwrap(),
        });
        let t = render(&mut app, 60, 16);
        let buf = t.backend().buffer();
        let areas = layout(Rect::new(0, 0, 60, 16), 2);
        let (x, y) = (areas.pane_inner.x, areas.pane_inner.y);
        assert_eq!(buf[(x, y)].symbol(), "r");
        assert_eq!(buf[(x, y)].fg, Color::Rgb(0xc0, 0x39, 0x2b), "Moda's red");
        assert_eq!(
            buf[(x, y)].bg,
            Color::Rgb(0xfb, 0xf4, 0xe8),
            "Moda's ground"
        );
        assert_eq!(buf[(x + 1, y)].fg, Color::Rgb(1, 2, 3));
        let inverse = &buf[(x + 2, y)];
        assert_eq!(inverse.fg, Color::Rgb(0x2b, 0x25, 0x30));
        assert_eq!(inverse.bg, Color::Rgb(0xfb, 0xf4, 0xe8));
        assert!(inverse.modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn statuses_take_the_themes_colours() {
        let mut app = with_theme(with_a_closed_project(), "uskudar");
        let t = render(&mut app, 60, 16);
        let buf = t.backend().buffer();
        assert_eq!(
            buf[(34, 0)].fg,
            app.theme.status(AgentStatus::NeedsFeedback)
        );
        assert_eq!(buf[(34, 0)].fg, Color::Rgb(0xff, 0x7a, 0x6b));
        assert_eq!(buf[(27, 0)].fg, app.theme.dim.fg.unwrap());
    }

    #[test]
    fn footers_follow_the_keymap() {
        let keys = termist_core::config::KeysConfig {
            grid: [
                ("g", "quick_prompt"),
                ("p", "none"),
                ("Left", "left"),
                ("h", "none"),
            ]
            .into_iter()
            .map(|(k, a)| (k.to_string(), a.to_string()))
            .collect(),
            focus: [("g", "grid")]
                .into_iter()
                .map(|(k, a)| (k.to_string(), a.to_string()))
                .collect(),
        };
        let (keymap, problems) = Keymap::from_config(&keys, "C-Space");
        assert!(problems.is_empty());
        let grid = grid_hint(&keymap);
        assert!(
            grid.starts_with("? help · g new task · Space follow-up"),
            "{grid}"
        );
        assert!(!grid.contains("p new task"));
        assert_eq!(
            archive_hint(&keymap),
            "archive · Enter restore and resume · Left/j/k/l move · d delete · A/Esc back · q quit"
        );
        assert_eq!(
            focus_hint(&keymap),
            "typing into the session · C-Space Esc grid · C-Space . next● · C-q grid"
        );
        assert_eq!(
            prefix_hint(&keymap),
            "C-Space …  Esc grid · . , next/prev● · hjkl move · C-Space literal"
        );
        let mut app = App::new();
        app.keymap = keymap;
        app.on_event(ServerEvent::State(StateSnapshot {
            projects: vec![ProjectInfo {
                id: ProjectId::new(),
                name: "web".into(),
                path: "/w".into(),
                open: true,
            }],
            ..StateSnapshot::default()
        }));
        let t = render(&mut app, 70, 10);
        assert_eq!(
            row(&t, 1),
            "No sessions yet.  g: new task  ·  n: agent  ·  t: shell"
        );
    }

    #[test]
    fn the_help_lists_every_key() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Char('?')));
        insta::assert_snapshot!(render(&mut app, 80, 40).backend());
    }

    #[test]
    fn the_help_shows_a_rebound_key_and_scrolls_to_its_end_only() {
        use ratatui::crossterm::event::KeyCode as K;
        let keys = termist_core::config::KeysConfig {
            grid: [("g", "quick_prompt"), ("p", "none")]
                .into_iter()
                .map(|(k, a)| (k.to_string(), a.to_string()))
                .collect(),
            ..Default::default()
        };
        let mut app = fixture();
        app.keymap = Keymap::from_config(&keys, "C-Space").0;
        app.on_key(key(K::Char('?')));
        let text = screen_text(&render(&mut app, 80, 60));
        assert!(
            text.contains("g            new task: prompt, CLI, model"),
            "{text}"
        );
        assert!(text.contains("Focus mode, after C-Space"));
        assert!(text.contains("C-Space C-Space"));
        for _ in 0..200 {
            app.on_key(key(K::Char('j')));
        }
        let text = screen_text(&render(&mut app, 80, 16));
        assert!(
            text.contains("[keys.grid] and [keys.focus]"),
            "the last line is in view"
        );
        app.on_key(key(K::Esc));
        assert!(app.overlays.is_empty());
    }

    #[test]
    fn the_help_opens_from_focus_mode_and_closes_back_to_it() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(key(K::Char('?')));
        assert!(matches!(
            app.overlays.last(),
            Some(crate::overlay::Overlay::Help { .. })
        ));
        app.on_key(key(K::Char('?')));
        assert!(app.overlays.is_empty());
        assert_eq!(app.mode, Mode::Focus);
    }
}
