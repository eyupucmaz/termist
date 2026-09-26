//! Drawing the overlay stack: each overlay is a box centred over the body, drawn
//! bottom to top, so a picker opened from the quick prompt sits on top of it.
use crate::app::App;
use crate::overlay::{BrowseEntry, Overlay, QuickPrompt};
use crate::text_input::TextInput;
use crate::ui::status_style;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// A box of `width` × `height` centred in `body`, clamped to it.
pub fn centered(body: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(body.width);
    let h = height.min(body.height);
    Rect {
        x: body.x + body.width.saturating_sub(w) / 2,
        y: body.y + body.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

/// Clears `area` and draws a bordered box titled `title` holding `lines`.
fn boxed(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
        ),
        area,
    );
}

fn highlighted(style: Style, on: bool) -> Style {
    if on {
        style.add_modifier(Modifier::REVERSED)
    } else {
        style
    }
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// A list in a box: a query line when the list is typed into, the rows (scrolled to
/// keep the highlight in view) and extra lines under them.
struct ListBox {
    title: String,
    width: u16,
    query: Option<String>,
    rows: Vec<Line<'static>>,
    highlight: usize,
    extra: Vec<Line<'static>>,
}

fn draw_list(f: &mut Frame, body: Rect, list: ListBox) {
    let fixed = list.query.is_some() as u16 + list.extra.len() as u16 + 2;
    let area = centered(body, list.width, fixed + (list.rows.len() as u16).max(1));
    let room = area.height.saturating_sub(fixed).max(1) as usize;
    let first = list.highlight.saturating_sub(room - 1);
    let mut lines = Vec::new();
    if let Some(q) = list.query {
        lines.push(Line::from(Span::styled(format!("> {q}"), dim())));
    }
    lines.extend(list.rows.into_iter().skip(first).take(room));
    lines.extend(list.extra);
    boxed(f, area, &list.title, lines);
}

/// The lines of `input` that fit `width` × `height`, scrolled to keep the cursor in
/// view, and the cursor's position inside that window.
fn input_view(input: &TextInput, width: u16, height: usize) -> (Vec<Line<'static>>, (u16, u16)) {
    let (line, col) = input.cursor_line_col();
    let first = line.saturating_sub(height.saturating_sub(1));
    let skip = (col + 1).saturating_sub(width.max(1) as usize);
    let lines = input
        .text()
        .split('\n')
        .skip(first)
        .take(height)
        .map(|l| Line::from(l.chars().skip(skip).collect::<String>()))
        .collect();
    (lines, ((col - skip) as u16, (line - first) as u16))
}

/// A one-line text box; the cursor shows when it is the top overlay.
fn text_box(f: &mut Frame, body: Rect, title: &str, width: u16, input: &TextInput, top: bool) {
    let area = centered(body, width, 3);
    let (lines, (cx, _)) = input_view(input, area.width.saturating_sub(2), 1);
    boxed(f, area, title, lines);
    if top && area.height == 3 {
        f.set_cursor_position((area.x + 1 + cx, area.y + 1));
    }
}

/// `orbit-api ^P · claude Tab · opus · high ^O`
pub fn launch_line(app: &App, q: &QuickPrompt) -> String {
    let project = app
        .state
        .projects
        .iter()
        .find(|p| p.id == q.project)
        .map_or("?", |p| p.name.as_str());
    let harness = q.launch.harness;
    let missing = if app
        .harnesses
        .iter()
        .any(|h| h.harness == harness && h.available)
    {
        ""
    } else {
        " (not installed)"
    };
    let model = q.launch.model.as_deref().unwrap_or("default");
    let effort = q
        .launch
        .effort
        .as_deref()
        .map(|e| format!(" · {e}"))
        .unwrap_or_default();
    format!(
        "{project} ^P · {}{missing} Tab · {model}{effort} ^O",
        harness.id()
    )
}

/// Draws one overlay; `top` is the one that gets the keys (and the cursor).
pub fn draw(f: &mut Frame, app: &App, overlay: &Overlay, body: Rect, top: bool) {
    match overlay {
        Overlay::Harness(picker) => {
            let rows = picker
                .visible()
                .map(|(i, h, on)| {
                    let note = if h.available { "" } else { "not installed" };
                    let style = if h.available { Style::default() } else { dim() };
                    Line::from(Span::styled(
                        format!(" {} {:<9} {note}", i + 1, h.harness.id()),
                        highlighted(style, on),
                    ))
                })
                .collect();
            draw_list(
                f,
                body,
                ListBox {
                    title: "new session".into(),
                    width: 36,
                    query: None,
                    rows,
                    highlight: picker.highlight(),
                    extra: vec![],
                },
            );
        }
        Overlay::QuickPrompt(q) => {
            let height = q.input.text().split('\n').count().clamp(3, 8);
            let area = centered(body, 72, height as u16 + 3);
            let inner_w = area.width.saturating_sub(2);
            let rows = area.height.saturating_sub(3) as usize;
            let (mut lines, (cx, cy)) = input_view(&q.input, inner_w, rows);
            lines.resize(rows, Line::default());
            lines.push(Line::from(Span::styled(launch_line(app, q), dim())));
            boxed(f, area, "new task", lines);
            if top && area.height > 3 {
                f.set_cursor_position((area.x + 1 + cx, area.y + 1 + cy));
            }
        }
        Overlay::Model(m) => {
            let rows = m
                .models
                .visible()
                .map(|(_, c, on)| {
                    Line::from(Span::styled(
                        format!(" {}", c.label()),
                        highlighted(Style::default(), on),
                    ))
                })
                .collect();
            let mut extra = vec![];
            if !m.harness.efforts().is_empty() {
                let mut spans = vec![Span::raw(" effort ")];
                let levels = std::iter::once("default").chain(m.harness.efforts().iter().copied());
                for (i, level) in levels.enumerate() {
                    spans.push(Span::styled(
                        format!(" {level} "),
                        highlighted(Style::default(), i == m.effort),
                    ));
                }
                extra = vec![Line::default(), Line::from(spans)];
            }
            draw_list(
                f,
                body,
                ListBox {
                    title: format!("model · {}", m.harness.id()),
                    width: 56,
                    query: None,
                    rows,
                    highlight: m.models.highlight(),
                    extra,
                },
            );
        }
        Overlay::ModelName(input) => text_box(f, body, "model name", 48, input, top),
        Overlay::FollowUp { session, input } => {
            let name = app
                .state
                .sessions
                .iter()
                .find(|s| s.id == *session)
                .map_or("?", |s| s.display_name());
            text_box(f, body, &format!("follow-up · {name}"), 64, input, top);
        }
        Overlay::Rename { input, .. } => text_box(f, body, "rename", 48, input, top),
        Overlay::Palette(picker) => {
            let rows = picker
                .visible()
                .filter_map(|(_, id, on)| {
                    let s = app.state.sessions.iter().find(|s| s.id == *id)?;
                    let project = app
                        .state
                        .projects
                        .iter()
                        .find(|p| p.id == s.project)
                        .map_or("", |p| p.name.as_str());
                    let (glyph, color, word) = status_style(s.status);
                    Some(Line::from(vec![
                        Span::styled(format!(" {glyph} "), Style::default().fg(color)),
                        Span::styled(
                            format!("{project:<14} {:<24}", s.display_name()),
                            highlighted(Style::default(), on),
                        ),
                        Span::styled(format!(" {} · {word}", s.kind.label()), dim()),
                    ]))
                })
                .collect();
            draw_list(
                f,
                body,
                ListBox {
                    title: "sessions".into(),
                    width: 64,
                    query: picker.query().map(str::to_string),
                    rows,
                    highlight: picker.highlight(),
                    extra: vec![],
                },
            );
        }
        Overlay::OpenProject(open) => {
            let rows = open
                .list
                .visible()
                .map(|(_, entry, on)| match entry {
                    BrowseEntry::Project(p) => Line::from(vec![
                        Span::styled(format!(" {}", p.name), highlighted(Style::default(), on)),
                        Span::styled(
                            format!(
                                "  {}{}",
                                if p.open { "" } else { "closed · " },
                                p.path.display()
                            ),
                            dim(),
                        ),
                    ]),
                    BrowseEntry::Dir(d) => Line::from(vec![
                        Span::styled(
                            if d.git { " ● " } else { "   " },
                            Style::default().fg(Color::Green),
                        ),
                        Span::styled(format!("{}/", d.name), highlighted(Style::default(), on)),
                    ]),
                })
                .collect();
            let note = if let Some(e) = &open.error {
                Some(Span::styled(
                    format!(" {e}"),
                    Style::default().fg(Color::Red),
                ))
            } else if open.loading {
                Some(Span::styled(" reading…", dim()))
            } else if open.truncated {
                Some(Span::styled(
                    " more folders than shown: type to narrow",
                    dim(),
                ))
            } else {
                None
            };
            draw_list(
                f,
                body,
                ListBox {
                    title: format!("open project · {}", open.dir.display()),
                    width: 64,
                    query: open.list.query().map(str::to_string),
                    rows,
                    highlight: open.list.highlight(),
                    extra: note.map(Line::from).into_iter().collect(),
                },
            );
        }
        Overlay::Project(picker) => {
            let rows = picker
                .visible()
                .map(|(_, p, on)| {
                    Line::from(vec![
                        Span::styled(format!(" {}", p.name), highlighted(Style::default(), on)),
                        Span::styled(format!("  {}", p.path.display()), dim()),
                    ])
                })
                .collect();
            draw_list(
                f,
                body,
                ListBox {
                    title: "project".into(),
                    width: 56,
                    query: picker.query().map(str::to_string),
                    rows,
                    highlight: picker.highlight(),
                    extra: vec![],
                },
            );
        }
    }
}

/// The footer line while `overlay` is on top.
pub fn hint(overlay: &Overlay) -> &'static str {
    match overlay {
        Overlay::Harness(_) => "j/k choose · Enter start · 1-3 pick · Esc cancel",
        Overlay::QuickPrompt(_) => {
            "Enter start · Shift+Enter newline · ↑ history · Tab CLI · ^O model · ^P project · Esc cancel"
        }
        Overlay::Model(m) if m.harness.efforts().is_empty() => {
            "j/k model · Enter choose · Esc back"
        }
        Overlay::Model(_) => "j/k model · h/l effort · Enter choose · Esc back",
        Overlay::ModelName(_) => "Enter use this model · Esc back",
        Overlay::Project(_) => "type to filter · ↑/↓ choose · Enter pick · Esc back",
        Overlay::FollowUp { .. } => "Enter send to the agent · Esc cancel",
        Overlay::Rename { .. } => "Enter rename · Esc cancel",
        Overlay::Palette(_) => "type to filter · ↑/↓ choose · Enter go there · Esc close",
        Overlay::OpenProject(_) => {
            "type to filter · Enter open · → in · ← up · Tab open this folder · Esc close"
        }
    }
}
