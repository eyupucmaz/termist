//! Drawing the overlay stack: each overlay is a box centred over the body, drawn
//! bottom to top, so a picker opened from the quick prompt sits on top of it.
use crate::app::App;
use crate::overlay::Overlay;
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

pub fn draw(f: &mut Frame, _app: &App, overlay: &Overlay, body: Rect) {
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
    }
}

/// The footer line while `overlay` is on top.
pub fn hint(overlay: &Overlay) -> &'static str {
    match overlay {
        Overlay::Harness(_) => "j/k choose · Enter start · 1-3 pick · Esc cancel",
    }
}
