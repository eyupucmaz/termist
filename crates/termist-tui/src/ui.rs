//! Rendering: header, cards, live pane and footer.
use crate::app::{App, Mode, View};
use crate::keys::{Action, Context, Keymap};
use crate::overlay_view;
use crate::scene_view::{self, ShowKind, Showing};
use crate::selection::Selection;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use std::time::Instant;
use termist_core::config::PanePosition;
use termist_core::{AgentStatus, Snapshot, cell_flags};
use termist_scenes::TimeOfDay;

const CARD_W: u16 = 24;
const CARD_H: u16 = 4;

pub struct Areas {
    pub header: Rect,
    /// Everything between the header and the footer: cards and pane.
    pub body: Rect,
    /// Where cards and their "more" lines go: above the pane, or left of it.
    pub cards_zone: Rect,
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
    /// The pane is beside the cards (right or left), not above or below them.
    pub pane_beside: bool,
}

/// From this many columns up, `auto` puts the pane right of the cards.
pub const PANE_RIGHT_FROM: u16 = 180;

pub fn layout(area: Rect, session_count: usize, position: PanePosition) -> Areas {
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
    let beside = match position {
        PanePosition::Right | PanePosition::Left => true,
        PanePosition::Bottom | PanePosition::Top => false,
        PanePosition::Auto => area.width >= PANE_RIGHT_FROM,
    };
    // The pane first (left or above), the cards after it.
    let pane_first = matches!(position, PanePosition::Left | PanePosition::Top);
    let (cards_zone, cards_per_row) = if beside {
        // One column of cards; the rest of the width is the pane's.
        let width = CARD_W.min(body.width);
        let x = if pane_first {
            body.right() - width
        } else {
            body.x
        };
        (Rect { x, width, ..body }, 1)
    } else {
        let per_row = (body.width / CARD_W).max(1) as usize;
        let card_rows = session_count.max(1).div_ceil(per_row) as u16;
        let height = (card_rows * CARD_H).min(body.height / 2);
        let y = if pane_first {
            body.bottom() - height
        } else {
            body.y
        };
        (Rect { y, height, ..body }, per_row)
    };
    let card_rows = session_count.max(1).div_ceil(cards_per_row) as u16;
    let scroll_lines = card_rows * CARD_H > cards_zone.height;
    let (cards, visible_rows) = if scroll_lines {
        let rows = (cards_zone.height.saturating_sub(2) / CARD_H).max(1);
        let cards = Rect {
            y: cards_zone.y + 1,
            height: (rows * CARD_H).min(cards_zone.height.saturating_sub(1)),
            ..cards_zone
        };
        (cards, rows)
    } else {
        (
            Rect {
                height: card_rows * CARD_H,
                ..cards_zone
            },
            card_rows,
        )
    };
    let pane = match (beside, pane_first) {
        (true, false) => Rect {
            x: cards_zone.right(),
            width: body.width - cards_zone.width,
            ..body
        },
        (true, true) => Rect {
            width: body.width - cards_zone.width,
            ..body
        },
        (false, false) => Rect {
            y: cards_zone.bottom(),
            height: body.height - cards_zone.height,
            ..body
        },
        (false, true) => Rect {
            height: body.height - cards_zone.height,
            ..body
        },
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
        cards_zone,
        cards,
        pane,
        pane_inner,
        footer,
        cards_per_row,
        card_rows: visible_rows as usize,
        scroll_lines,
        pane_beside: beside,
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

/// "Galata Kulesi · gece", under a scene.
fn scene_caption(app: &App, scene: &termist_scenes::Scene) -> Line<'static> {
    let when = match app.time_of_day() {
        TimeOfDay::Sabah => "sabah",
        TimeOfDay::Gunduz => "gündüz",
        TimeOfDay::Aksam => "gün batımı",
        TimeOfDay::Gece => "gece",
    };
    Line::from(Span::styled(
        format!("{} · {when}", scene.title),
        app.theme.dim,
    ))
}

/// A scene in `area` with `caption` under it; the wordmark when it is not loaded.
fn draw_scene(f: &mut Frame, app: &App, name: &str, area: Rect, caption: Vec<Line<'static>>) {
    let n = app.scene_frame(Instant::now());
    match app.scenes.get(name) {
        Some(scene) => {
            scene_view::draw(
                f.buffer_mut(),
                area,
                scene,
                app.time_of_day(),
                n,
                &app.theme,
                scene_caption(app, scene),
                &caption,
            );
        }
        None => scene_view::wordmark(f.buffer_mut(), area, &app.theme, &caption),
    }
}

pub fn draw(f: &mut Frame, app: &App, areas: &Areas) {
    let area = f.area();
    *app.hits.borrow_mut() = crate::hit::Hits::default();
    f.buffer_mut().set_style(area, app.theme.base);
    match app.showing {
        Some(Showing {
            kind: ShowKind::Splash,
            name,
            ..
        }) => {
            let version = Line::from(Span::styled(
                format!("termist {}", env!("CARGO_PKG_VERSION")),
                app.theme.dim,
            ));
            draw_scene(f, app, name, area, vec![version]);
            return;
        }
        Some(Showing {
            kind: ShowKind::Idle,
            name,
            ..
        }) => {
            draw_header(f, app, areas.header);
            draw_scene(f, app, name, areas.body, vec![]);
            f.render_widget(
                Paragraph::new("any key: back").style(app.theme.dim),
                areas.footer,
            );
            return;
        }
        None => {}
    }
    draw_header(f, app, areas.header);
    if let View::Prs(view) = &app.view {
        crate::prs::draw(f, app, view, areas.body);
        for (i, overlay) in app.overlays.iter().enumerate() {
            overlay_view::draw(f, app, overlay, areas.body, i + 1 == app.overlays.len());
        }
        draw_footer(f, app, areas.footer);
        draw_toasts(f, app);
        return;
    }
    let sessions = app.project_sessions();
    if sessions.is_empty() && app.connected && !app.archive_view() {
        let hint = empty_hint(app);
        draw_scene(
            f,
            app,
            app.scene,
            areas.body,
            vec![Line::from(Span::styled(hint, app.theme.dim))],
        );
    } else if sessions.is_empty() {
        let text = if !app.connected {
            "Connecting to the termist daemon…".to_string()
        } else {
            match app.keymap.key(Context::Grid, Action::ArchiveView) {
                Some(a) => format!("Nothing archived in this project.  {a}: back"),
                None => "Nothing archived in this project.  Esc: back".to_string(),
            }
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
            app.hits.borrow_mut().cards.push((s.id, rect));
        }
        app.hits.borrow_mut().cards_zone = areas.cards_zone;
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
                if n > 0 && rect.y >= areas.cards_zone.y && rect.y < areas.cards_zone.bottom() {
                    f.render_widget(
                        Paragraph::new(format!("{arrow} {n} more")).style(app.theme.dim),
                        rect,
                    );
                    let mut hits = app.hits.borrow_mut();
                    if arrow == '↑' {
                        hits.above = Some(rect);
                    } else {
                        hits.below = Some(rect);
                    }
                }
            }
        }
        draw_pane(f, app, areas);
    }
    for (i, overlay) in app.overlays.iter().enumerate() {
        overlay_view::draw(f, app, overlay, areas.body, i + 1 == app.overlays.len());
    }
    draw_footer(f, app, areas.footer);
    draw_toasts(f, app);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        " termist ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if app.archive_view() {
        spans.push(Span::styled(
            "archive ",
            app.theme.archive.add_modifier(Modifier::BOLD),
        ));
    }
    if matches!(app.view, View::Prs(_)) {
        spans.push(Span::styled(
            "pull requests ",
            app.theme.accent.add_modifier(Modifier::BOLD),
        ));
    }
    let width = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    let mut tabs = Vec::new();
    for p in app.open_projects() {
        let style = if Some(p.id) == app.project {
            app.theme.tab_active
        } else {
            Style::default()
        };
        spans.push(Span::raw(" "));
        let from = width(&spans);
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
        let asked = app.prs.get(&p.id).map_or(0, |d| {
            d.repos
                .iter()
                .flat_map(|r| &r.prs)
                .filter(|pr| pr.requested_you && !pr.draft)
                .count()
        });
        if asked > 0 && app.config.github.enabled {
            spans.push(Span::styled(
                format!("⇄{asked}"),
                Style::default().fg(app.theme.status(AgentStatus::NeedsFeedback)),
            ));
        }
        let to = width(&spans);
        if from < area.width as usize {
            tabs.push((
                p.id,
                area.x + from as u16,
                area.x + to.min(area.width as usize) as u16,
            ));
        }
    }
    {
        let mut hits = app.hits.borrow_mut();
        hits.tabs = tabs;
        hits.header = area;
    }
    // Agents waiting in closed projects; the first thing to go when space is short.
    let waiting = app.waiting_in_closed_projects().len();
    if waiting > 0 {
        let (glyph, color, _) = status_style(&app.theme, AgentStatus::NeedsFeedback);
        let marker = [
            Span::styled("  closed ", app.theme.dim),
            Span::styled(format!("{glyph}{waiting}"), Style::default().fg(color)),
        ];
        if width(&spans) + width(&marker) <= area.width as usize {
            spans.extend(marker);
        }
    }
    let used: usize = spans.iter().map(Span::width).sum();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let room = (area.width as usize).saturating_sub(used + 1);
    let status = crate::status_line::spans(
        &app.sysstat,
        &app.config.status,
        (app.hour, app.minute),
        &app.theme,
        room,
    );
    let width: u16 = status.iter().map(|s| s.width() as u16).sum();
    if width > 0 {
        f.render_widget(
            Paragraph::new(Line::from(status)),
            Rect {
                x: area.right() - width,
                width,
                ..area
            },
        );
    }
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
    let screen = app.screens.get(&info.id);
    let back = screen.map_or(0, |s| s.scroll.offset);
    let selection = app.selection.filter(|s| s.session == info.id);
    let state = match screen {
        Some(s) if app.scrolling || back > 0 => {
            format!(" · ↑ {}/{}", s.scroll.offset, s.scroll.history)
        }
        _ if focused => " · typing".to_string(),
        _ => String::new(),
    };
    let title = format!(" {} — {}{state} ", info.display_name(), info.kind.label());
    let border = if focused || app.scrolling {
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
        if let Some(selection) = selection {
            draw_selection(f.buffer_mut(), areas.pane_inner, &selection);
        }
        let c = screen.cursor;
        // An overlay on top has the keys; a text box places its own cursor.
        if focused
            && !app.scrolling
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

/// Selected cells swap colours: what is drawn inverse comes back plain.
fn draw_selection(buf: &mut Buffer, area: Rect, selection: &Selection) {
    for row in 0..area.height {
        for col in 0..area.width {
            if selection.contains(col, row) {
                let cell = &mut buf[(area.x + col, area.y + row)];
                cell.modifier.toggle(Modifier::REVERSED);
            }
        }
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
    let prs_view = match &app.view {
        View::Prs(v) => Some(v),
        _ => None,
    };
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
        (_, Mode::Grid | Mode::Focus) if app.scrolling => (SCROLL_HINT.to_string(), t.focus),
        (_, Mode::Grid) if prs_view.is_some() => (crate::prs::hint(app, prs_view.unwrap()), t.dim),
        (_, Mode::Grid) if app.archive_view() => (archive_hint(&app.keymap), t.dim),
        (_, Mode::Grid) => (grid_hint(&app.keymap), t.dim),
        (_, Mode::Focus) => (focus_hint(&app.keymap), t.dim),
        (_, Mode::FocusPrefix) => (prefix_hint(&app.keymap), t.focus),
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

/// The toasts, over everything but a scene.
fn draw_toasts(f: &mut Frame, app: &App) {
    let area = f.area();
    let t = &app.theme;
    for (toast, rect) in app.toasts.items().zip(app.toasts.rects(area)) {
        let text = crate::toast::fit(&toast.text);
        let line = match toast.kind {
            crate::toast::ToastKind::Agent { status, .. } => {
                let (_, color, _) = status_style(t, status);
                let mut chars = text.chars();
                let mark: String = chars.next().into_iter().collect();
                Line::from(vec![
                    Span::styled(mark, Style::default().fg(color)),
                    Span::raw(chars.collect::<String>()),
                ])
            }
            crate::toast::ToastKind::Copied
            | crate::toast::ToastKind::Review { .. }
            | crate::toast::ToastKind::Failed => {
                let mut chars = text.chars();
                let mark: String = chars.next().into_iter().collect();
                let style = match toast.kind {
                    crate::toast::ToastKind::Failed => t.error,
                    _ => t.accent,
                };
                Line::from(vec![
                    Span::styled(mark, style),
                    Span::raw(chars.collect::<String>()),
                ])
            }
        };
        f.render_widget(ratatui::widgets::Clear, rect);
        f.render_widget(
            Paragraph::new(line).style(t.base).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(t.border)
                    .padding(ratatui::widgets::Padding::horizontal(1)),
            ),
            rect,
        );
    }
}

/// What to do on an empty grid.
fn empty_hint(app: &App) -> String {
    let key = |action| app.keymap.key(Context::Grid, action);
    if app.state.projects.is_empty() {
        "No project yet: run termist inside a project folder.".to_string()
    } else if app.project.is_none() {
        match key(Action::OpenProject) {
            Some(o) => format!("No project open · {o} opens one"),
            None => "No project open".to_string(),
        }
    } else {
        let hints: Vec<String> = [Action::QuickPrompt, Action::NewSession, Action::NewShell]
            .into_iter()
            .filter_map(|a| Some(format!("{}: {}", key(a)?, a.hint())))
            .collect();
        format!("No sessions yet.  {}", hints.join("  ·  "))
    }
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

/// The footer while the pane shows history; these keys are fixed.
const SCROLL_HINT: &str =
    "history · ↑↓ j/k line · PgUp/PgDn page · C-u/C-d half · g top · q/Esc/G back to live";

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
            PullRequests,
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
        let areas = layout(
            Rect::new(0, 0, w, h),
            app.project_sessions().len(),
            app.pane_position(),
        );
        app.pane_beside = areas.pane_beside;
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
        let text = screen_text(&render(&mut app, 60, 10));
        assert!(text.contains("No project yet: run termist inside a project folder."));
        assert!(
            text.contains(scene_view::WORDMARK),
            "too small for the scene"
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
            for position in PanePosition::ALL {
                let areas = layout(Rect::new(0, 0, w, h), 2, position);
                t.draw(|f| draw(f, &app, &areas)).unwrap();
                app.pane_resized(areas.pane_inner.width, areas.pane_inner.height);
            }
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
    fn a_follow_up_box_wraps_and_grows_with_its_lines() {
        use ratatui::crossterm::event::{KeyCode as K, KeyEvent, KeyModifiers as M};
        let mut app = fixture();
        let mut info = app.selected_info().unwrap().clone();
        info.status = AgentStatus::Running;
        app.on_event(ServerEvent::SessionUpdated(info));
        app.on_key(key(K::Char(' ')));
        let typed =
            |app: &mut App, s: &str| s.chars().for_each(|c| _ = app.on_key(key(K::Char(c))));
        typed(
            &mut app,
            "the tests pass now, so look at the login redirect again and keep it short",
        );
        app.on_key(KeyEvent::new(K::Enter, M::ALT));
        typed(&mut app, "then commit");
        insta::assert_snapshot!(render(&mut app, 80, 20).backend());
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
        assert_eq!(
            row(&t, 0).strip_suffix("12:00").unwrap().trim_end(),
            " termist   closed ◆1",
            "its agent still waits"
        );
        assert!(screen_text(&t).contains("No project open · o opens one"));
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
            row(&t, 0).strip_suffix("12:00").unwrap().trim_end(),
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
        assert_eq!(
            row(&t, 0).strip_suffix("12:00").unwrap().trim_end(),
            " termist   orbit-api ◆1✓1  closed ◆2"
        );
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
        assert_eq!(
            row(&t, 0).strip_suffix("12:00").unwrap().trim_end(),
            " termist   orbit-api ◆1✓1",
            "none waiting"
        );
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
        let areas = layout(Rect::new(0, 0, 60, 16), 2, PanePosition::Auto);
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

    /// Selected cells swap their colours, an inverse one back to plain; the title
    /// says what went on the clipboard.
    #[test]
    fn a_selection_is_drawn_inverted() {
        let mut app = fixture();
        let id = app.selected.unwrap();
        let mut snap = Snapshot::blank(10, 2);
        snap.lines[0][1].flags = cell_flags::INVERSE;
        let before = app.screens[&id].clone();
        app.on_event(ServerEvent::Screen {
            session: id,
            update: diff(Some(&before), &snap).unwrap(),
        });
        render(&mut app, 60, 16);
        app.selection = Some(crate::selection::Selection {
            session: id,
            anchor: (0, 0),
            head: (2, 0),
        });
        let t = render(&mut app, 60, 16);
        let buf = t.backend().buffer();
        let areas = layout(Rect::new(0, 0, 60, 16), 2, PanePosition::Auto);
        let (x, y) = (areas.pane_inner.x, areas.pane_inner.y);
        assert!(buf[(x, y)].modifier.contains(Modifier::REVERSED));
        assert!(!buf[(x + 1, y)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(x + 2, y)].modifier.contains(Modifier::REVERSED));
        assert!(!buf[(x + 3, y)].modifier.contains(Modifier::REVERSED));
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
        assert!(
            screen_text(&t).contains("No sessions yet.  g: new task  ·  n: agent  ·  t: shell")
        );
    }

    #[test]
    fn the_help_lists_every_key() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Char('?')));
        // The version is left out: it changes with every release, and so does its
        // length, so the padding after it goes too.
        let text = render(&mut app, 80, 40)
            .backend()
            .to_string()
            .replace(env!("CARGO_PKG_VERSION"), "<version>")
            .lines()
            .map(|line| match line.rfind('│') {
                Some(edge) if line.contains("<version>") => {
                    format!("{} {}", line[..edge].trim_end(), &line[edge..])
                }
                _ => line.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(text);
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
        render(&mut app, 80, 16);
        for _ in 0..200 {
            app.on_key(key(K::Char('j')));
        }
        let text = screen_text(&render(&mut app, 80, 16));
        assert!(
            text.contains("[keys.grid] and [keys.focus]"),
            "the last line is in view"
        );
        app.on_key(key(K::Char('k')));
        let text = screen_text(&render(&mut app, 80, 16));
        assert!(
            !text.contains("[keys.grid] and [keys.focus]"),
            "one k from the end moves the view back"
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

    #[test]
    fn the_settings_and_the_keys_screens() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.on_key(key(K::Char('s')));
        insta::assert_snapshot!("settings", render(&mut app, 80, 16).backend());
        for _ in 0..4 {
            app.on_key(key(K::Char('j')));
        }
        app.on_key(key(K::Enter));
        app.on_key(key(K::Enter));
        app.on_key(key(K::Char('g')));
        insta::assert_snapshot!("keys", render(&mut app, 80, 16).backend());
    }

    #[test]
    fn auto_puts_the_pane_on_the_right_from_180_columns() {
        let area = |w| Rect::new(0, 0, w, 40);
        assert!(!layout(area(179), 3, PanePosition::Auto).pane_beside);
        let wide = layout(area(180), 3, PanePosition::Auto);
        assert!(wide.pane_beside);
        assert_eq!(wide.cards_per_row, 1, "beside the pane, one column");
        assert_eq!(wide.pane.x, 24);
        assert_eq!(wide.pane.width, 156);
        assert_eq!(wide.pane.height, 38, "the whole body");
        assert!(layout(area(100), 3, PanePosition::Right).pane_beside);
        assert!(!layout(area(200), 3, PanePosition::Bottom).pane_beside);
    }

    #[test]
    fn the_pane_on_the_right() {
        let mut app = fixture();
        app.config.pane_position = PanePosition::Right;
        insta::assert_snapshot!(render(&mut app, 80, 12).backend());
    }

    #[test]
    fn cards_that_do_not_fit_left_of_the_pane_are_counted() {
        let mut app = fixture();
        app.config.pane_position = PanePosition::Right;
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
        let t = render(&mut app, 80, 16);
        let left = |y| {
            row(&t, y)
                .chars()
                .take(24)
                .collect::<String>()
                .trim()
                .to_string()
        };
        assert_eq!(left(14), "↓ 4 more", "under the three cards that fit");
        assert_eq!(left(1), "", "none above");
    }

    #[test]
    fn the_pane_on_the_left_or_on_top() {
        let area = Rect::new(0, 0, 100, 30);
        let left = layout(area, 3, PanePosition::Left);
        assert!(left.pane_beside);
        assert_eq!((left.cards_zone.x, left.cards_zone.width), (76, 24));
        assert_eq!((left.pane.x, left.pane.width), (0, 76));
        assert_eq!(left.cards_per_row, 1);
        let top = layout(area, 3, PanePosition::Top);
        assert!(!top.pane_beside);
        assert_eq!(top.pane.y, top.body.y, "the pane first");
        assert_eq!(top.pane.bottom(), top.cards_zone.y);
        assert_eq!(top.cards_zone.bottom(), top.body.bottom());
        let mut app = fixture();
        app.config.pane_position = PanePosition::Left;
        insta::assert_snapshot!("pane_left", render(&mut app, 80, 12).backend());
        app.config.pane_position = PanePosition::Top;
        insta::assert_snapshot!("pane_top", render(&mut app, 60, 16).backend());
    }

    #[test]
    fn prefix_z_keeps_the_pane_on_its_side() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        app.config.pane_position = PanePosition::Left;
        render(&mut app, 100, 20);
        app.on_key(key(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(key(K::Char('z')));
        assert_eq!(app.pane_position(), PanePosition::Top);
        render(&mut app, 100, 20);
        app.on_key(ctrl('a'));
        app.on_key(key(K::Char('z')));
        assert_eq!(app.pane_position(), PanePosition::Left);
    }

    #[test]
    fn prefix_z_moves_the_pane_until_termist_quits() {
        use ratatui::crossterm::event::KeyCode as K;
        let mut app = fixture();
        render(&mut app, 100, 20);
        assert!(!app.pane_beside);
        app.on_key(key(K::Enter));
        app.on_key(ctrl('a'));
        app.on_key(key(K::Char('z')));
        assert_eq!(app.pane_position(), PanePosition::Right);
        let t = render(&mut app, 100, 20);
        assert!(
            t.backend().buffer()[(24, 1)].symbol() == "┌",
            "the pane starts right of the cards"
        );
        app.pane_beside = true;
        app.on_key(ctrl('a'));
        app.on_key(key(K::Char('z')));
        assert_eq!(app.pane_position(), PanePosition::Bottom);
        assert_eq!(
            app.config.pane_position,
            PanePosition::Auto,
            "the setting is untouched"
        );
    }

    #[test]
    fn a_toast_is_drawn_in_the_top_right_corner() {
        let mut app = fixture();
        app.toasts.push(crate::toast::Toast {
            text: "✓ copied 5 characters".into(),
            kind: crate::toast::ToastKind::Copied,
            until: std::time::Instant::now() + std::time::Duration::from_secs(2),
        });
        let t = render(&mut app, 80, 20);
        // 21 characters, a space and a border on each side: columns 54-78, then one free.
        assert!(
            row(&t, 1).ends_with("╭───────────────────────╮"),
            "{:?}",
            row(&t, 1)
        );
        assert!(
            row(&t, 2).ends_with("│ ✓ copied 5 characters │"),
            "{:?}",
            row(&t, 2)
        );
        assert!(
            row(&t, 3).ends_with("╰───────────────────────╯"),
            "{:?}",
            row(&t, 3)
        );
        let buf = t.backend().buffer();
        assert_eq!(buf[(78, 2)].symbol(), "│");
        assert_eq!(buf[(54, 2)].symbol(), "│");
    }

    #[test]
    fn no_toast_over_a_scene() {
        let mut app = fixture();
        app.start_splash(std::time::Instant::now());
        app.toasts.push(crate::toast::Toast {
            text: "✓ copied 5 characters".into(),
            kind: crate::toast::ToastKind::Copied,
            until: std::time::Instant::now() + std::time::Duration::from_secs(2),
        });
        let t = render(&mut app, 80, 20);
        assert!(!screen_text(&t).contains("copied"));
    }

    #[test]
    fn the_header_ends_with_the_status_line() {
        let mut app = fixture();
        app.sysstat = termist_platform::sysstat::SysStat {
            cpu: Some(5.0),
            ram: None,
            battery: None,
        };
        app.hour = 14;
        app.minute = 32;
        let t = render(&mut app, 100, 20);
        assert!(
            row(&t, 0).trim_end().ends_with("cpu 5%  14:32"),
            "{:?}",
            row(&t, 0)
        );
    }

    use crate::prs::fixtures::{repo, summary};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use termist_core::github::{Checks, GhState, Mergeable, ReviewDecision, unix_secs};

    fn screen(t: &Terminal<TestBackend>) -> String {
        let buf = t.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The fixture's project with four pull requests in two repos, the PR view open,
    /// the clock at noon on 2026-10-02.
    fn pr_fixture() -> App {
        let mut app = fixture();
        let project = app.state.projects[0].id;
        let mut review = summary(212, "Add a dealer filter to search", "bob");
        review.requested_you = true;
        review.unseen = true;
        let mut failing = summary(209, "Fix image lazy loading", "alice");
        failing.checks = Checks::Failing;
        failing.decision = Some(ReviewDecision::ChangesRequested);
        failing.updated_at = "2026-10-01T12:00:00Z".into();
        let mut wip = summary(201, "WIP: new header", "alice");
        wip.draft = true;
        wip.updated_at = "2026-09-27T12:00:00Z".into();
        let mut conflict = summary(140, "Paginate /vehicles", "carol");
        conflict.mergeable = Mergeable::Conflicting;
        conflict.decision = Some(ReviewDecision::Approved);
        conflict.updated_at = "2026-10-02T06:00:00Z".into();
        app.on_event(ServerEvent::Prs {
            project,
            state: GhState::Ok,
            discovered: 3,
            repos: vec![
                repo(1, "site", vec![review, failing, wip]),
                repo(2, "admin-api", vec![conflict]),
            ],
        });
        app.frozen_now = unix_secs("2026-10-02T12:00:00Z");
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        app
    }

    fn open_detail(app: &mut App) {
        let project = app.state.projects[0].id;
        let pr = app.prs[&project].repos[0].prs[0].clone();
        let pr_ref = termist_core::github::PrRef {
            repo: termist_core::github::RepoId(1),
            number: pr.number,
        };
        app.on_event(ServerEvent::PrDetail {
            pr: pr_ref,
            state: GhState::Ok,
            detail: Some(Box::new(crate::prs::fixtures::detail(pr))),
        });
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    }

    #[test]
    fn a_pull_request_whole() {
        let mut app = pr_fixture();
        open_detail(&mut app);
        insta::assert_snapshot!("pr_overview", render(&mut app, 90, 16).backend());
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        insta::assert_snapshot!("pr_conversation", render(&mut app, 90, 28).backend());
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let text = screen(&render(&mut app, 90, 16));
        assert!(text.contains("✗ lint"), "failing first: {text}");
        assert!(text.contains("1m 12s"));
    }

    fn click(app: &mut App, x: u16, y: u16) -> Vec<crate::app::Action> {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let ev = |kind| MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let mut actions = app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left)));
        actions.extend(app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left))));
        actions
    }

    /// The column where `text` starts on row `y` of the last frame.
    fn column_of(t: &Terminal<TestBackend>, y: u16, text: &str) -> u16 {
        let row: Vec<String> = {
            let buf = t.backend().buffer();
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        };
        (0..row.len())
            .find(|&x| row[x..].concat().starts_with(text))
            .unwrap_or_else(|| panic!("{text} not on row {y}: {}", row.concat())) as u16
    }

    /// The fixture with a second project, `web`, empty.
    fn two_tabs() -> (App, termist_core::ProjectId) {
        let mut app = fixture();
        let web = ProjectInfo {
            id: ProjectId::new(),
            name: "web".into(),
            path: "/web".into(),
            open: true,
        };
        let mut state = app.state.clone();
        state.projects.push(web.clone());
        app.on_event(ServerEvent::State(state));
        (app, web.id)
    }

    #[test]
    fn clicking_a_tab_goes_to_its_project_from_any_view() {
        let (mut app, web) = two_tabs();
        let orbit = app.state.projects[0].id;
        let t = render(&mut app, 100, 20);
        click(&mut app, column_of(&t, 0, "web"), 0);
        assert_eq!(app.project, Some(web));
        let t = render(&mut app, 100, 20);
        click(&mut app, column_of(&t, 0, "orbit-api") + 3, 0);
        assert_eq!(app.project, Some(orbit));
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        let t = render(&mut app, 100, 20);
        let actions = click(&mut app, column_of(&t, 0, "web"), 0);
        assert_eq!(app.project, Some(web));
        assert!(matches!(&app.view, View::Prs(v) if v.project == Some(web)));
        assert!(actions.iter().any(|a| matches!(
            a,
            crate::app::Action::Send(termist_core::ClientRequest::SetPrFocus { project: Some(p), .. }) if *p == web
        )));
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.mode, Mode::Focus);
        let t = render(&mut app, 100, 20);
        click(&mut app, column_of(&t, 0, "web"), 0);
        assert_eq!((app.mode, app.project), (Mode::Grid, Some(web)));
    }

    fn mouse_at(app: &mut App, kind: ratatui::crossterm::event::MouseEventKind, x: u16, y: u16) {
        app.on_mouse(ratatui::crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn a_card_click_selects_and_a_second_one_goes_in() {
        let mut app = fixture();
        let shell = app.state.sessions[1].id;
        let t = render(&mut app, 60, 16);
        let x = column_of(&t, 2, "shell-2");
        click(&mut app, x, 2);
        assert_eq!((app.selected, app.mode), (Some(shell), Mode::Grid));
        render(&mut app, 60, 16);
        click(&mut app, x, 2);
        assert_eq!(app.mode, Mode::Focus);
        let t = render(&mut app, 60, 16);
        click(&mut app, column_of(&t, 2, "claude-1"), 2);
        assert_eq!(
            (app.selected, app.mode),
            (Some(app.state.sessions[0].id), Mode::Grid),
            "out of focus, onto the card clicked"
        );
    }

    #[test]
    fn a_click_on_the_pane_goes_in_and_a_drag_still_selects() {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind as Kind};
        let mut app = fixture();
        let areas = layout(Rect::new(0, 0, 60, 16), 2, app.pane_position());
        app.pane_area = areas.pane_inner;
        render(&mut app, 60, 16);
        let (x, y) = (areas.pane_inner.x + 1, areas.pane_inner.y);
        mouse_at(&mut app, Kind::Down(MouseButton::Left), x, y);
        mouse_at(&mut app, Kind::Drag(MouseButton::Left), x + 3, y);
        mouse_at(&mut app, Kind::Up(MouseButton::Left), x + 3, y);
        assert_eq!(app.mode, Mode::Grid, "a drag copies, it does not go in");
        click(&mut app, x, y);
        assert_eq!(app.mode, Mode::Focus);
    }

    #[test]
    fn the_wheel_and_the_more_lines_move_through_the_cards() {
        use ratatui::crossterm::event::MouseEventKind as Kind;
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
        let first = app.selected;
        mouse_at(&mut app, Kind::ScrollDown, 3, 3);
        assert_eq!(app.selected, Some(app.state.sessions[2].id), "a row down");
        mouse_at(&mut app, Kind::ScrollUp, 3, 3);
        assert_eq!(app.selected, first);
        let t = render(&mut app, 60, 16);
        let below = (1..16)
            .find(|y| screen(&t).lines().nth(*y as usize).unwrap().contains("↓"))
            .unwrap();
        click(&mut app, 3, below);
        assert_eq!(app.selected, Some(app.state.sessions[2].id));
    }

    /// 212 open in Mercek on DealerFilter.tsx (thread T1 on its new line 42), the one
    /// file not viewed; client.ts (T2, resolved, on line 10), a binary logo and a README
    /// are.
    fn mercek_fixture() -> App {
        use termist_core::github::{DiffFile, Patch, PrDiff, Viewed};
        let mut app = pr_fixture();
        open_detail(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        let f = |path: &str, change: char, patch: Patch| DiffFile {
            path: path.into(),
            previous: None,
            change,
            additions: 3,
            deletions: 1,
            viewed: Viewed::Unviewed,
            patch,
            url: format!("https://github.com/acme/site/pull/212/files#diff-{path}"),
        };
        let mut diff = PrDiff {
            head_oid: "h1".into(),
            files: vec![
                f(
                    "src/search/DealerFilter.tsx",
                    'A',
                    Patch::Text(
                        "@@ -38,4 +38,6 @@ export function DealerFilter\n   const dealers = useDealers();\n   const [sel, setSel] = useState<string>();\n-  const label = 'All';\n+  const label = sel ?? 'All';\n+  useEffect(() => fetchAll(), []);\n+\treturn label;\n   return (\n".into(),
                    ),
                ),
                f(
                    "src/api/client.ts",
                    'M',
                    Patch::Text("@@ -8,3 +8,4 @@\n a\n b\n+c\n d".into()),
                ),
                f("public/logo.png", 'A', Patch::Binary),
                f(
                    "README.md",
                    'M',
                    Patch::Text("@@ -1 +1 @@\n-old\n+new".into()),
                ),
            ],
            more: 0,
        };
        for f in &mut diff.files[1..] {
            f.viewed = Viewed::Viewed;
        }
        app.on_event(ServerEvent::PrDiff {
            pr: termist_core::github::PrRef {
                repo: termist_core::github::RepoId(1),
                number: 212,
            },
            state: GhState::Ok,
            diff: Some(Box::new(diff)),
        });
        app
    }

    /// The row of the last frame that holds `text`.
    fn row_of(t: &Terminal<TestBackend>, text: &str) -> u16 {
        screen(t)
            .lines()
            .position(|l| l.contains(text))
            .unwrap_or_else(|| panic!("{text} not on screen: {}", screen(t))) as u16
    }

    fn detail(app: &App) -> &crate::prs::Detail {
        match &app.view {
            View::Prs(v) => v.detail.as_ref().unwrap(),
            _ => panic!("not in the PR view"),
        }
    }

    #[test]
    fn the_detail_takes_clicks_on_tabs_threads_checks_and_files() {
        use crate::prs::Tab;
        let mut app = pr_fixture();
        open_detail(&mut app);
        let t = render(&mut app, 90, 28);
        let y = row_of(&t, "Conversation");
        click(&mut app, column_of(&t, y, "Conversation"), y);
        assert_eq!(detail(&app).tab, Tab::Conversation);
        let t = render(&mut app, 90, 28);
        let y = row_of(&t, "src/api/client.ts:10");
        click(&mut app, 5, y);
        assert!(
            detail(&app).toggled.contains("T2"),
            "a resolved thread unfolds"
        );
        let t = render(&mut app, 90, 28);
        click(
            &mut app,
            column_of(&t, row_of(&t, "Checks"), "Checks"),
            row_of(&t, "Checks"),
        );
        let t = render(&mut app, 90, 28);
        click(&mut app, 5, row_of(&t, "e2e"));
        assert_eq!(detail(&app).check, 1, "failing first, then running");
        let t = render(&mut app, 90, 28);
        let y = row_of(&t, "Files");
        click(&mut app, column_of(&t, y, "Files"), y);
        let t = render(&mut app, 90, 28);
        let y = row_of(&t, "src/api/client.ts");
        click(&mut app, 8, y);
        assert_eq!(detail(&app).file.as_deref(), Some("src/api/client.ts"));
        assert!(detail(&app).diff.is_none());
        let actions = click(&mut app, 8, y);
        assert_eq!(
            detail(&app).diff.as_ref().and_then(|d| d.file.as_deref()),
            Some("src/api/client.ts"),
            "a second click opens its diff"
        );
        assert!(actions.iter().any(|a| matches!(
            a,
            crate::app::Action::Send(termist_core::ClientRequest::SetPrFocus { diff: true, .. })
        )));
    }

    #[test]
    fn mercek_takes_clicks_and_the_wheel_where_they_are() {
        use crate::prs::diff::Panel;
        use ratatui::crossterm::event::MouseEventKind as Kind;
        let mut app = mercek_fixture();
        let open = |app: &App| detail(app).diff.clone().unwrap();
        let t = render(&mut app, 110, 18);
        let y = row_of(&t, "client.ts");
        click(&mut app, column_of(&t, y, "client.ts"), y);
        assert_eq!(open(&app).file.as_deref(), Some("src/api/client.ts"));
        assert_eq!(open(&app).panel, Panel::Tree, "the tree stays in use");
        let t = render(&mut app, 110, 18);
        let y = row_of(&t, "search/");
        click(&mut app, 5, y);
        assert!(open(&app).folded.contains("src/search"));
        click(&mut app, 5, y);
        let t = render(&mut app, 110, 18);
        let y = row_of(&t, "DealerFilter");
        click(&mut app, 8, y);
        let t = render(&mut app, 110, 18);
        let y = row_of(&t, "carol +1");
        click(&mut app, 60, y);
        assert_eq!(open(&app).panel, Panel::Diff);
        assert!(open(&app).opened.contains("T1"));
        // Short enough that the file with its open thread scrolls.
        render(&mut app, 110, 12);
        let cursor = open(&app).cursor;
        mouse_at(&mut app, Kind::ScrollDown, 5, 5);
        assert_eq!(open(&app).cursor, cursor + 1, "over the tree: its cursor");
        mouse_at(&mut app, Kind::ScrollDown, 60, 5);
        assert_eq!(open(&app).scroll, 3, "over the diff: three lines");
        assert_eq!(
            open(&app).panel,
            Panel::Diff,
            "the wheel does not change the panel"
        );
    }

    #[test]
    fn the_help_lists_the_diff_and_the_mouse() {
        let app = fixture();
        let text: String = crate::overlay_view::help_lines(&app)
            .iter()
            .map(|l| l.to_string() + "\n")
            .collect();
        assert!(text.contains("A pull request's diff (d)"));
        assert!(text.contains("mark the file viewed on GitHub"));
        assert!(text.contains("Mouse"));
    }

    #[test]
    fn mercek_says_why_when_the_pull_request_cannot_be_read() {
        let mut app = pr_fixture();
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        app.on_event(ServerEvent::PrDetail {
            pr: termist_core::github::PrRef {
                repo: termist_core::github::RepoId(1),
                number: 212,
            },
            state: GhState::NoAccess,
            detail: None,
        });
        let text = screen(&render(&mut app, 110, 14));
        assert!(
            text.contains("No logged-in account can see this repo."),
            "{text}"
        );
        assert!(!text.contains("Reading the diff"));
    }

    #[test]
    fn mercek_unified_and_split() {
        let mut app = mercek_fixture();
        insta::assert_snapshot!("mercek_unified", render(&mut app, 110, 18).backend());
        let layout = app.pr_layout.borrow().diff.clone();
        assert_eq!(layout.hunks, [0]);
        assert_eq!(
            layout
                .threads
                .iter()
                .map(|t| t.1.as_str())
                .collect::<Vec<_>>(),
            ["T1"]
        );
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        insta::assert_snapshot!("mercek_split", render(&mut app, 150, 18).backend());
        let narrow = screen(&render(&mut app, 110, 18));
        assert!(narrow.contains("split · too narrow"), "{narrow}");
    }

    #[test]
    fn mercek_on_a_narrow_screen_shows_one_panel() {
        let mut app = mercek_fixture();
        let text = screen(&render(&mut app, 80, 14));
        assert!(!text.contains(" files "), "the tree is hidden: {text}");
        assert!(text.contains("DealerFilter.tsx"));
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        insta::assert_snapshot!("mercek_tree_alone", render(&mut app, 80, 14).backend());
    }

    #[test]
    fn mercek_opens_a_thread_and_tells_a_file_without_a_patch() {
        let mut app = mercek_fixture();
        render(&mut app, 110, 18);
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let text = screen(&render(&mut app, 110, 24));
        assert!(
            text.contains("This refetches on every mount, can we memoize?"),
            "{text}"
        );
        assert!(text.contains("Good catch, will fix."));
        app.on_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT));
        app.on_key(KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT));
        let text = screen(&render(&mut app, 110, 18));
        assert!(text.contains("logo.png"));
        assert!(text.contains("binary file"), "{text}");
    }

    #[test]
    fn n_jumps_to_the_open_thread_after_a_frame() {
        let mut app = pr_fixture();
        open_detail(&mut app);
        app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        render(&mut app, 90, 12);
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        let layout = app.pr_layout.borrow().clone();
        let t1 = layout.threads.iter().find(|a| a.id == "T1").unwrap().line;
        let View::Prs(v) = &app.view else { panic!() };
        assert_eq!(v.detail.as_ref().unwrap().scroll, t1.min(layout.end));
    }

    #[test]
    fn a_screen_without_the_list_leaves_no_stale_layout() {
        let mut app = pr_fixture();
        render(&mut app, 110, 16);
        assert_ne!(app.pr_layout.borrow().list, Rect::default());
        let project = app.state.projects[0].id;
        app.prs.get_mut(&project).unwrap().state = GhState::NoGh;
        render(&mut app, 110, 16);
        assert_eq!(app.pr_layout.borrow().list, Rect::default());
    }

    #[test]
    fn the_inbox_beside_the_selected_pr() {
        let mut app = pr_fixture();
        insta::assert_snapshot!(render(&mut app, 110, 16).backend());
    }

    #[test]
    fn a_narrow_inbox_is_only_the_list() {
        let mut app = pr_fixture();
        let text = screen(&render(&mut app, 70, 12));
        assert!(text.contains("#212"));
        assert!(
            !text.contains("bob · feat → main"),
            "no preview below 100 columns"
        );
        insta::assert_snapshot!(render(&mut app, 70, 12).backend());
    }

    #[test]
    fn a_long_title_is_cut_to_the_row() {
        let mut app = pr_fixture();
        let project = app.state.projects[0].id;
        let data = app.prs.get_mut(&project).unwrap();
        data.repos[0].prs[0].title = "word ".repeat(60);
        let text = screen(&render(&mut app, 70, 12));
        let row = text.lines().find(|l| l.contains("#212")).unwrap();
        assert!(row.contains('…'), "{row}");
        assert!(row.contains("2h"), "the age stays: {row}");
    }

    #[test]
    fn without_gh_the_inbox_says_how_to_get_it() {
        let mut app = pr_fixture();
        let project = app.state.projects[0].id;
        app.prs.get_mut(&project).unwrap().state = GhState::NoGh;
        let text = screen(&render(&mut app, 80, 12));
        assert!(
            text.contains("Install gh, then run: gh auth login"),
            "{text}"
        );
    }

    #[test]
    fn the_tab_bar_counts_reviews_asked_of_you() {
        let mut app = pr_fixture();
        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        let header = screen(&render(&mut app, 80, 12))
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert!(header.contains("⇄1"), "{header}");
        assert!(!header.contains("pull requests"), "back on the grid");
    }

    #[test]
    fn the_repos_window() {
        let mut app = pr_fixture();
        app.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        let project = app.state.projects[0].id;
        use termist_core::github::{RepoId, RepoInfo};
        app.on_event(ServerEvent::Repos {
            project,
            accounts: vec!["work".into()],
            repos: vec![
                RepoInfo {
                    id: RepoId(2),
                    name: "admin-api".into(),
                    slug: "acme/admin-api".into(),
                    visible: true,
                    account: Some("work".into()),
                    pinned: true,
                    open_count: Some(1),
                    state: GhState::Ok,
                },
                RepoInfo {
                    id: RepoId(3),
                    name: "discord".into(),
                    slug: "acme/discord".into(),
                    visible: false,
                    account: None,
                    pinned: false,
                    open_count: None,
                    state: GhState::NoAccess,
                },
            ],
        });
        insta::assert_snapshot!(render(&mut app, 80, 14).backend());
    }

    #[test]
    fn a_repo_not_read_yet_says_so_not_that_it_has_nothing() {
        let mut app = pr_fixture();
        let project = app.state.projects[0].id;
        let mut fresh = repo(3, "discord", vec![]);
        fresh.fetched_at = None;
        fresh.viewer = None;
        let mut read = repo(4, "docs", vec![]);
        read.total = 0;
        app.on_event(ServerEvent::Prs {
            project,
            state: GhState::Ok,
            discovered: 2,
            repos: vec![fresh, read],
        });
        let text = screen(&render(&mut app, 80, 10));
        let line = |name: &str| {
            let lines: Vec<&str> = text.lines().collect();
            let at = lines.iter().position(|l| l.contains(name)).unwrap();
            lines[at + 1].to_string()
        };
        assert!(line("discord").contains("reading…"), "{text}");
        assert!(line("docs").contains("no open pull requests"), "{text}");
    }

    #[test]
    fn the_repos_window_tells_not_asked_yet_from_no_access() {
        use termist_core::github::{RepoId, RepoInfo};
        let mut app = pr_fixture();
        app.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        let project = app.state.projects[0].id;
        let info = |id: i64, name: &str, state: GhState| RepoInfo {
            id: RepoId(id),
            name: name.into(),
            slug: format!("acme/{name}"),
            visible: true,
            account: None,
            pinned: false,
            open_count: None,
            state,
        };
        app.on_event(ServerEvent::Repos {
            project,
            accounts: vec!["work".into()],
            repos: vec![
                info(1, "asking", GhState::Ok),
                info(2, "hidden", GhState::NoAccess),
                info(3, "broken", GhState::Failed("HTTP 502".into())),
            ],
        });
        let row =
            |text: &str, name: &str| text.lines().find(|l| l.contains(name)).unwrap().to_string();
        let text = screen(&render(&mut app, 80, 14));
        assert!(row(&text, "asking").contains('…'), "{text}");
        assert!(!row(&text, "asking").contains("no access"), "{text}");
        assert!(row(&text, "hidden").contains("no access"), "{text}");
        assert!(row(&text, "broken").contains("HTTP 502"), "{text}");
        // GitHub as a whole is the trouble: its reason, not "…".
        app.prs.get_mut(&project).unwrap().state = GhState::LoggedOut;
        let text = screen(&render(&mut app, 80, 14));
        assert!(row(&text, "asking").contains("logged out"), "{text}");
    }

    #[test]
    fn the_wheel_moves_through_the_inbox_and_a_click_opens() {
        use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = pr_fixture();
        render(&mut app, 110, 16);
        let at = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(at(MouseEventKind::ScrollDown, 5, 5));
        let View::Prs(v) = &app.view else { panic!() };
        assert_eq!(v.selected.map(|p| p.number), Some(209));
        let list = app.pr_layout.borrow().list;
        // rows: site, #212, #209, #201 → #201 is the fourth
        let y = list.y + 3 - app.pr_layout.borrow().first as u16;
        app.on_mouse(at(MouseEventKind::Down(MouseButton::Left), list.x + 4, y));
        let View::Prs(v) = &app.view else { panic!() };
        assert_eq!(v.selected.map(|p| p.number), Some(201));
        assert!(v.detail.is_none(), "the first click selects");
        app.on_mouse(at(MouseEventKind::Down(MouseButton::Left), list.x + 4, y));
        let View::Prs(v) = &app.view else { panic!() };
        assert_eq!(
            v.detail.as_ref().map(|d| d.pr.number),
            Some(201),
            "a click on the selected one opens it"
        );
    }
}
