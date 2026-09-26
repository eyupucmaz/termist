//! Rendering: header, cards, live pane and footer.
use crate::app::{App, Mode};
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
    pub cards: Rect,
    pub pane: Rect,
    pub pane_inner: Rect,
    pub footer: Rect,
    pub cards_per_row: usize,
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
    let cards_h = (card_rows * CARD_H).min(body.height / 2);
    let cards = Rect {
        height: cards_h,
        ..body
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
        cards,
        pane,
        pane_inner,
        footer,
        cards_per_row,
    }
}

pub fn status_style(status: AgentStatus) -> (char, Color, &'static str) {
    match status {
        AgentStatus::Fresh => ('●', Color::DarkGray, "fresh"),
        AgentStatus::Running => ('●', Color::Yellow, "running"),
        AgentStatus::Unseen => ('✓', Color::Blue, "done"),
        AgentStatus::Finished => ('●', Color::Green, "ready"),
        AgentStatus::NeedsFeedback => ('◆', Color::Red, "waiting"),
        AgentStatus::Exited { code: Some(0) } => ('●', Color::DarkGray, "closed"),
        AgentStatus::Exited { .. } => ('✗', Color::Magenta, "exited"),
        AgentStatus::Disconnected => ('○', Color::Gray, "disconnected"),
    }
}

pub fn draw(f: &mut Frame, app: &App, areas: &Areas) {
    draw_header(f, app, areas.header);
    let sessions = app.project_sessions();
    if sessions.is_empty() {
        let text = if !app.connected {
            "Connecting to the termist daemon…"
        } else if app.state.projects.is_empty() {
            "No project yet: run termist inside a project folder."
        } else {
            "No sessions yet.  n: new claude  ·  t: new shell"
        };
        let body = Rect {
            height: areas.cards.height + areas.pane.height,
            ..areas.cards
        };
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            body,
        );
    } else {
        for (i, s) in sessions.iter().enumerate() {
            let col = (i % areas.cards_per_row) as u16;
            let row = (i / areas.cards_per_row) as u16;
            let rect = Rect {
                x: areas.cards.x + col * CARD_W,
                y: areas.cards.y + row * CARD_H,
                width: CARD_W,
                height: CARD_H,
            };
            if rect.bottom() > areas.cards.bottom() || rect.right() > areas.cards.right() {
                continue;
            }
            draw_card(f, s, Some(s.id) == app.selected, rect);
        }
        draw_pane(f, app, areas);
    }
    if let Mode::PickHarness(selected) = app.mode {
        draw_picker(
            f,
            app,
            selected,
            Rect {
                height: areas.cards.height + areas.pane.height,
                ..areas.cards
            },
        );
    }
    draw_footer(f, app, areas.footer);
}

fn draw_picker(f: &mut Frame, app: &App, selected: usize, body: Rect) {
    let w = 36.min(body.width);
    let h = (app.harnesses.len() as u16 + 2).min(body.height);
    let area = Rect {
        x: body.x + body.width.saturating_sub(w) / 2,
        y: body.y + body.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    };
    let lines: Vec<Line> = app
        .harnesses
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let note = if h.available { "" } else { "not installed" };
            let mut style = if h.available {
                Style::default()
            } else {
                Style::default().fg(Color::DarkGray)
            };
            if i == selected {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Line::from(Span::styled(
                format!(" {} {:<9} {note}", i + 1, h.harness.id()),
                style,
            ))
        })
        .collect();
    f.render_widget(ratatui::widgets::Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" new session "),
        ),
        area,
    );
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        " termist ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    for p in &app.state.projects {
        let style = if Some(p.id) == app.project {
            Style::default().add_modifier(Modifier::REVERSED)
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
                .filter(|s| s.project == p.id && s.status == status)
                .count();
            if n > 0 {
                let (glyph, color, _) = status_style(status);
                spans.push(Span::styled(
                    format!("{glyph}{n}"),
                    Style::default().fg(color),
                ));
            }
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_card(f: &mut Frame, s: &termist_core::SessionInfo, selected: bool, rect: Rect) {
    let (glyph, color, word) = status_style(s.status);
    let border = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if selected {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(border);
    let name = s.title.as_deref().unwrap_or(&s.name);
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(color)),
            Span::raw(name.to_string()),
        ]),
        Line::from(Span::styled(
            format!("{} · {word}", s.kind.label()),
            Style::default().fg(Color::DarkGray),
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
        info.name,
        info.kind.label(),
        if focused { " · typing" } else { "" }
    );
    let border = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border),
        areas.pane,
    );
    if let Some(screen) = app.screens.get(&info.id) {
        render_screen(f.buffer_mut(), areas.pane_inner, screen);
        let c = screen.cursor;
        if focused
            && screen.modes.show_cursor
            && c.col < areas.pane_inner.width
            && c.row < areas.pane_inner.height
        {
            f.set_cursor_position((areas.pane_inner.x + c.col, areas.pane_inner.y + c.row));
        }
    } else if info.status == AgentStatus::Disconnected {
        f.render_widget(
            Paragraph::new("Not running. Enter resumes this session.")
                .style(Style::default().fg(Color::DarkGray)),
            areas.pane_inner,
        );
    }
}

fn render_screen(buf: &mut Buffer, area: Rect, screen: &Snapshot) {
    for (r, line) in screen.lines.iter().enumerate().take(area.height as usize) {
        for (c, cell) in line.iter().enumerate().take(area.width as usize) {
            if cell.flags & cell_flags::WIDE_SPACER != 0 {
                continue;
            }
            let mut style = Style::default().fg(color(cell.fg)).bg(color(cell.bg));
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

fn color(c: termist_core::Color) -> Color {
    match c {
        termist_core::Color::Default => Color::Reset,
        termist_core::Color::Indexed(i) => Color::Indexed(i),
        termist_core::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let (text, style) = match (&app.message, app.mode) {
        (Some(m), Mode::Grid) => (m.clone(), Style::default().fg(Color::Red)),
        (_, Mode::ConfirmQuit) => (
            "Leave termist? Sessions keep running in the daemon.  y / Enter: quit · any key: stay"
                .into(),
            Style::default().fg(Color::Yellow),
        ),
        (_, Mode::ConfirmKill(id)) => {
            let name = app
                .state
                .sessions
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.as_str())
                .unwrap_or("this session");
            (
                format!("Kill {name}? It stops the process.  y / Enter: kill · any key: cancel"),
                Style::default().fg(Color::Yellow),
            )
        }
        (_, Mode::Grid) => (
            "n new agent · t shell · Enter focus/resume · . next● · hjkl move · d kill · q quit"
                .into(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Mode::Focus) => (
            "typing into the session · C-a Esc grid · C-a . next● · C-q grid".into(),
            Style::default().fg(Color::DarkGray),
        ),
        (_, Mode::FocusPrefix) => (
            "C-a …  Esc grid · . , next/prev● · hjkl move · C-a literal".into(),
            Style::default().fg(Color::Yellow),
        ),
        (_, Mode::PickHarness(_)) => (
            "j/k choose · Enter start · 1-3 pick · Esc cancel".into(),
            Style::default().fg(Color::Yellow),
        ),
    };
    f.render_widget(Paragraph::new(text).style(style), area);
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
        app.cards_per_row = areas.cards_per_row;
        app.pane_resized(areas.pane_inner.width, areas.pane_inner.height);
        t.draw(|f| draw(f, app, &areas)).unwrap();
        t
    }

    fn fixture() -> App {
        let p = ProjectInfo {
            id: ProjectId::new(),
            name: "orbit-api".into(),
            path: "/x".into(),
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
            }],
            sessions: vec![],
        }));
        insta::assert_snapshot!(render(&mut app, 60, 10).backend());
    }

    #[test]
    fn status_glyphs_follow_the_prd() {
        assert_eq!(status_style(AgentStatus::NeedsFeedback).0, '◆');
        assert_eq!(status_style(AgentStatus::Unseen).0, '✓');
        assert_eq!(status_style(AgentStatus::Disconnected).0, '○');
        assert_eq!(status_style(AgentStatus::Exited { code: Some(1) }).0, '✗');
        assert_eq!(status_style(AgentStatus::Running).2, "running");
    }

    // Review Focus: TestBackend's Display is text-only, so no snapshot would
    // catch a swapped or wrong colour. Pin the full (glyph, Color, word) tuple
    // for every status against the Global Constraints table.
    #[test]
    fn status_style_matches_the_global_table() {
        assert_eq!(
            status_style(AgentStatus::Fresh),
            ('●', Color::DarkGray, "fresh")
        );
        assert_eq!(
            status_style(AgentStatus::Running),
            ('●', Color::Yellow, "running")
        );
        assert_eq!(
            status_style(AgentStatus::Unseen),
            ('✓', Color::Blue, "done")
        );
        assert_eq!(
            status_style(AgentStatus::Finished),
            ('●', Color::Green, "ready")
        );
        assert_eq!(
            status_style(AgentStatus::NeedsFeedback),
            ('◆', Color::Red, "waiting")
        );
        assert_eq!(
            status_style(AgentStatus::Exited { code: Some(1) }),
            ('✗', Color::Magenta, "exited")
        );
        assert_eq!(
            status_style(AgentStatus::Exited { code: Some(0) }),
            ('●', Color::DarkGray, "closed")
        );
        assert_eq!(
            status_style(AgentStatus::Disconnected),
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

    // Review Focus 5
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

    // Review Focus 4
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
}
