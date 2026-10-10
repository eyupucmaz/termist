//! Drawing the overlay stack: each overlay is a box centred over the body, drawn
//! bottom to top, so a picker opened from the quick prompt sits on top of it.
use crate::app::App;
use crate::keys::{self, Action as KeyAction, Context};
use crate::overlay::{
    BrowseEntry, CaptureTarget, Overlay, QuickPrompt, SETTING_ROWS, SettingRow, key_rows,
};
use crate::scene_view;
use crate::text_input::TextInput;
use crate::theme::Theme;
use crate::ui::status_style;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use termist_core::config::{ColorDepth, DiffLayout, PanePosition, Sound};
use termist_core::github::GhState;

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
fn boxed(f: &mut Frame, theme: &Theme, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    boxed_in(f, theme, area, title, lines, None);
}

/// `boxed` with its border in `border` (a new worktree's green).
fn boxed_in(
    f: &mut Frame,
    theme: &Theme,
    area: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    border: Option<Style>,
) {
    f.render_widget(Clear, area);
    f.buffer_mut().set_style(area, theme.base);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    if let Some(border) = border {
        block = block.border_style(border);
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn highlighted(theme: &Theme, style: Style, on: bool) -> Style {
    if on {
        style.patch(theme.selection)
    } else {
        style
    }
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

fn draw_list(f: &mut Frame, theme: &Theme, body: Rect, list: ListBox) {
    let fixed = list.query.is_some() as u16 + list.extra.len() as u16 + 2;
    let area = centered(body, list.width, fixed + (list.rows.len() as u16).max(1));
    let room = area.height.saturating_sub(fixed).max(1) as usize;
    let first = list.highlight.saturating_sub(room - 1);
    let mut lines = Vec::new();
    if let Some(q) = list.query {
        lines.push(Line::from(Span::styled(format!("> {q}"), theme.dim)));
    }
    lines.extend(list.rows.into_iter().skip(first).take(room));
    lines.extend(list.extra);
    boxed(f, theme, area, &list.title, lines);
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

/// The text of `input` wrapped at `width` columns, after a space where a line has one:
/// its rows and the cursor's row and column. The last row of a line always has room
/// for the cursor after its last char.
fn wrapped_rows(input: &TextInput, width: u16) -> (Vec<String>, (usize, usize)) {
    let width = width.max(1) as usize;
    let (line, col) = input.cursor_line_col();
    let mut rows = Vec::new();
    let mut cursor = (0, 0);
    for (i, l) in input.text().split('\n').enumerate() {
        let chars: Vec<char> = l.chars().collect();
        let mut start = 0;
        loop {
            let end = if chars.len() - start < width {
                chars.len()
            } else {
                (start + 1..=start + width)
                    .rev()
                    .find(|&b| chars[b - 1] == ' ')
                    .unwrap_or(start + width)
            };
            let last = end == chars.len() && chars.len() - start < width;
            if i == line && col >= start && (col < end || last) {
                cursor = (rows.len(), col - start);
            }
            rows.push(chars[start..end].iter().collect());
            if last {
                break;
            }
            start = end;
        }
    }
    (rows, cursor)
}

/// A text box for a prompt: it wraps long lines and grows with the text from
/// `MIN_PROMPT_ROWS` to `MAX_PROMPT_ROWS` rows, then scrolls to keep the cursor in
/// view. `footer` is a dim line under the text.
fn prompt_box(
    f: &mut Frame,
    theme: &Theme,
    body: Rect,
    title: &str,
    input: &TextInput,
    footer: Option<String>,
    top: bool,
) {
    prompt_box_in(f, theme, body, title, input, footer, top, None);
}

#[allow(clippy::too_many_arguments)]
fn prompt_box_in(
    f: &mut Frame,
    theme: &Theme,
    body: Rect,
    title: &str,
    input: &TextInput,
    footer: Option<String>,
    top: bool,
    border: Option<Style>,
) {
    let width = PROMPT_WIDTH.min(body.width);
    let (all, (cy, cx)) = wrapped_rows(input, width.saturating_sub(2));
    let fixed = 2 + footer.is_some() as u16;
    let height = all.len().clamp(MIN_PROMPT_ROWS, MAX_PROMPT_ROWS) as u16 + fixed;
    let area = centered(body, width, height);
    let room = area.height.saturating_sub(fixed) as usize;
    let first = (cy + 1).saturating_sub(room);
    let mut lines: Vec<Line<'static>> = all
        .into_iter()
        .skip(first)
        .take(room)
        .map(Line::from)
        .collect();
    lines.resize(room, Line::default());
    if let Some(footer) = footer {
        lines.push(Line::from(Span::styled(footer, theme.dim)));
    }
    boxed_in(f, theme, area, title, lines, border);
    if top && room > 0 {
        f.set_cursor_position((area.x + 1 + cx as u16, area.y + 1 + (cy - first) as u16));
    }
}

const PROMPT_WIDTH: u16 = 72;
const MIN_PROMPT_ROWS: usize = 4;
const MAX_PROMPT_ROWS: usize = 10;

/// The box a comment or a review is written in: the lines commented on (or the
/// verdicts) over the text, and a line saying what is happening under it.
fn compose_box(f: &mut Frame, t: &Theme, body: Rect, c: &crate::prs::compose::Compose, top: bool) {
    use termist_core::github::Verdict;
    let width = PROMPT_WIDTH.min(body.width);
    let inner = width.saturating_sub(2) as usize;
    let mut head: Vec<Line<'static>> = c
        .context
        .iter()
        .map(|(n, mark, text)| {
            Line::from(Span::styled(
                crate::prs::markdown::cut(
                    &format!("{n:>5} {mark} {}", text.replace('\t', "    ")),
                    inner,
                ),
                t.dim,
            ))
        })
        .collect();
    if c.target == crate::prs::compose::Target::Submit {
        let mut spans = vec![Span::raw(" ")];
        for v in Verdict::ALL {
            let style = if v == c.verdict {
                t.tab_active
            } else if c.mine && v != Verdict::Comment {
                t.dim.add_modifier(Modifier::CROSSED_OUT)
            } else {
                t.dim
            };
            let label = if v == c.verdict {
                format!("‹ {} ›", v.label())
            } else {
                format!("  {}  ", v.label())
            };
            spans.push(Span::styled(label, style));
            spans.push(Span::raw(" "));
        }
        head.push(Line::from(spans));
    }
    if !head.is_empty() {
        head.push(Line::from(Span::styled("─".repeat(inner), t.border)));
    }
    let (all, (cy, cx)) = wrapped_rows(&c.input, width.saturating_sub(2));
    let fixed = 3 + head.len() as u16;
    let height =
        (all.len().clamp(MIN_PROMPT_ROWS, MAX_PROMPT_ROWS) as u16 + fixed).min(body.height);
    let area = centered(body, width, height);
    let room = area.height.saturating_sub(fixed) as usize;
    let first = (cy + 1).saturating_sub(room);
    let mut lines = head.clone();
    let mut text: Vec<Line<'static>> = all
        .into_iter()
        .skip(first)
        .take(room)
        .map(Line::from)
        .collect();
    text.resize(room, Line::default());
    lines.extend(text);
    let footer_style = match &c.state {
        crate::prs::compose::Sending::Failed(_) => t.error,
        _ => t.dim,
    };
    lines.push(Line::from(Span::styled(c.footer(), footer_style)));
    boxed(f, t, area, &c.title(), lines);
    if top && room > 0 && !matches!(c.state, crate::prs::compose::Sending::Sending(_)) {
        f.set_cursor_position((
            area.x + 1 + cx as u16,
            area.y + 1 + head.len() as u16 + (cy - first) as u16,
        ));
    }
}

/// A one-line text box; the cursor shows when it is the top overlay.
fn text_box(
    f: &mut Frame,
    theme: &Theme,
    body: Rect,
    title: &str,
    width: u16,
    input: &TextInput,
    top: bool,
) {
    let area = centered(body, width, 3);
    let (lines, (cx, _)) = input_view(input, area.width.saturating_sub(2), 1);
    boxed(f, theme, area, title, lines);
    if top && area.height == 3 {
        f.set_cursor_position((area.x + 1 + cx, area.y + 1));
    }
}

/// `orbit-api ^P · folder ^T · claude Tab · opus · high ^O`; `⎇ fix/login ^T` in a
/// worktree, `⎇ new ^N` for a new one.
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
    let worktree = match (&q.worktree, &q.new_worktree) {
        (_, Some(_)) => "⎇ new ^N".to_string(),
        (Some((_, branch)), None) => format!("⎇ {branch}"),
        (None, None) => "folder".to_string(),
    };
    format!(
        "{project} ^P · {worktree} ^T · {}{missing} Tab · {model}{effort} ^O",
        harness.id()
    )
}

/// Draws one overlay; `top` is the one that gets the keys (and the cursor).
pub fn draw(f: &mut Frame, app: &App, overlay: &Overlay, body: Rect, top: bool) {
    let t = &app.theme;
    let dim = || t.dim;
    let highlighted = |style, on| highlighted(t, style, on);
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
                t,
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
            // A new worktree: the box is green and says the branch it will be on.
            let (title, border) = match (&q.new_worktree, app.new_branch(q)) {
                (Some(new), Some(branch)) => {
                    let repo = new
                        .repo_name
                        .as_ref()
                        .map(|r| format!("{r}@"))
                        .unwrap_or_default();
                    (
                        format!("new worktree ⎇ {repo}{branch}"),
                        Some(Style::default().fg(t.status(termist_core::AgentStatus::Finished))),
                    )
                }
                _ => match &q.preset {
                    Some(p) => (format!("new task · {}", p.name), None),
                    None => ("new task".to_string(), None),
                },
            };
            let title = match &q.issue {
                Some(i) => format!("{title} · issue #{}", i.link.number),
                None => title,
            };
            prompt_box_in(
                f,
                t,
                body,
                &title,
                &q.input,
                Some(launch_line(app, q)),
                top,
                border,
            )
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
            let efforts = m.efforts();
            if !efforts.is_empty() {
                let mut spans = vec![Span::raw(" effort ")];
                let levels = std::iter::once("default".to_string()).chain(efforts);
                for (i, level) in levels.enumerate() {
                    spans.push(Span::styled(
                        format!(" {level} "),
                        highlighted(Style::default(), i == m.effort_index()),
                    ));
                }
                extra = vec![Line::default(), Line::from(spans)];
            }
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: format!("model · {}", m.harness.id()),
                    width: 64,
                    query: m.models.query().map(str::to_string),
                    rows,
                    highlight: m.models.highlight(),
                    extra,
                },
            );
        }
        Overlay::ModelName(input) => text_box(f, t, body, "model name", 48, input, top),
        Overlay::FollowUp { session, input } => {
            let name = app
                .state
                .sessions
                .iter()
                .find(|s| s.id == *session)
                .map_or("?", |s| s.display_name());
            prompt_box(f, t, body, &format!("follow-up · {name}"), input, None, top);
        }
        Overlay::Rename { input, .. } => text_box(f, t, body, "rename", 48, input, top),
        Overlay::PresetName {
            input, name_for, ..
        } => {
            let title = match name_for {
                crate::overlay::NameFor::Save(_) => "save preset as",
                crate::overlay::NameFor::Rename(_) => "rename preset",
            };
            text_box(f, t, body, title, 40, input, top)
        }
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
                    let (glyph, color, word) = status_style(t, s.status);
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
                t,
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
        Overlay::Finder(finder) => draw_finder(f, t, body, finder),
        Overlay::Presets { picker, .. } => {
            let available = |h: termist_core::Harness| {
                app.harnesses.iter().any(|i| i.harness == h && i.available)
            };
            let rows = picker
                .visible()
                .filter_map(|(_, name, on)| {
                    let p = app.config.presets.iter().find(|p| &p.name == name)?;
                    let setup = [
                        Some(p.harness.id()),
                        p.model.as_deref(),
                        p.effort.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                    let words: String = p.prefix.replace('\n', " ").chars().take(28).collect();
                    let style = if available(p.harness) {
                        Style::default()
                    } else {
                        t.dim
                    };
                    Some(Line::from(vec![
                        Span::styled(format!(" {:<12}", p.name), highlighted(style, on)),
                        Span::styled(format!(" {setup:<24}"), highlighted(t.dim, on)),
                        Span::styled(format!(" {words}"), highlighted(t.dim, on)),
                    ]))
                })
                .collect();
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: "presets".into(),
                    width: 72,
                    query: None,
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
                            Style::default().fg(t.status(termist_core::AgentStatus::Finished)),
                        ),
                        Span::styled(format!("{}/", d.name), highlighted(Style::default(), on)),
                    ]),
                })
                .collect();
            let note = if let Some(e) = &open.error {
                Some(Span::styled(format!(" {e}"), t.error))
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
                t,
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
        Overlay::Help { scroll } => {
            let mut lines = help_lines(app);
            let mut width = 78;
            // The About scene on top, where it fits.
            if let Some(scene) = app.scenes.get(app.scene)
                && app.theme.draws_scenes()
                && body.width as usize >= scene.width + 2
            {
                width = scene.width as u16 + 2;
                let n = app.scene_frame(std::time::Instant::now());
                let mut top = scene_view::lines(scene, app.time_of_day(), n, t);
                top.push(Line::from(Span::styled(format!(" {}", scene.title), dim())));
                top.push(Line::default());
                top.append(&mut lines);
                lines = top;
            }
            let area = centered(body, width, body.height);
            let room = area.height.saturating_sub(2) as usize;
            let end = lines.len().saturating_sub(room);
            app.help_end.set(end);
            let first = (*scroll).min(end);
            let shown = lines.into_iter().skip(first).take(room).collect();
            boxed(f, t, area, "help", shown);
        }
        Overlay::Settings(view) => {
            let depth = app.config.colors;
            let colours = match depth {
                ColorDepth::Auto => format!("auto ({} here)", depth_name(app.detected_depth)),
                other => depth_name(other).to_string(),
            };
            let local = |key: &str| {
                if app.local_settings.iter().any(|k| k == key) {
                    "  (config.local.toml)"
                } else {
                    ""
                }
            };
            let rows = SETTING_ROWS
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let (name, value) = match row {
                        SettingRow::Theme => (
                            "theme",
                            format!(
                                "‹ {} ›{}",
                                app.themes.name_of(&app.config.theme),
                                local("theme")
                            ),
                        ),
                        SettingRow::Colors => {
                            ("colours", format!("‹ {colours} ›{}", local("colors")))
                        }
                        SettingRow::Prefix => (
                            "prefix",
                            format!("{}{}", app.keymap.prefix, local("prefix")),
                        ),
                        SettingRow::Pane => {
                            let place = match app.config.pane_position {
                                PanePosition::Auto => "auto (right from 180 columns)",
                                PanePosition::Right => "right of the cards",
                                PanePosition::Left => "left of the cards",
                                PanePosition::Bottom => "under the cards",
                                PanePosition::Top => "above the cards",
                            };
                            ("pane", format!("‹ {place} ›{}", local("pane_position")))
                        }
                        SettingRow::Keys => ("keys", format!("…{}", local("keys"))),
                        SettingRow::DoneSound | SettingRow::WaitingSound => {
                            let (name, sound, key) = match row {
                                SettingRow::DoneSound => (
                                    "done sound",
                                    app.config.notify.done_sound,
                                    "notify.done_sound",
                                ),
                                _ => (
                                    "waiting sound",
                                    app.config.notify.waiting_sound,
                                    "notify.waiting_sound",
                                ),
                            };
                            let sound = match sound {
                                Sound::Marti => "martı (a seagull)",
                                Sound::Kedi => "kedi (a cat)",
                                Sound::System => "the system's",
                                Sound::Bell => "the terminal bell",
                                Sound::Off => "off",
                            };
                            let local = match local(key) {
                                "" => local("notify.sounds"),
                                set => set,
                            };
                            (name, format!("‹ {sound} ›{local}"))
                        }
                        SettingRow::Desktop => {
                            let on = if app.config.notify.desktop {
                                "on, when the terminal is not in front"
                            } else {
                                "off"
                            };
                            ("desktop", format!("‹ {on} ›{}", local("notify.desktop")))
                        }
                        SettingRow::Toasts => {
                            let on = if app.config.notify.toasts {
                                "on, for agents that wait or are done"
                            } else {
                                "off (a copy still shows one)"
                            };
                            ("toasts", format!("‹ {on} ›{}", local("notify.toasts")))
                        }
                        SettingRow::Splash => {
                            let on = if app.config.scenes.splash {
                                "on"
                            } else {
                                "off"
                            };
                            ("splash", format!("‹ {on} ›{}", local("scenes.splash")))
                        }
                        SettingRow::Idle => {
                            let idle = match app.config.scenes.idle_minutes {
                                0 => "off".to_string(),
                                m => format!("after {m} min"),
                            };
                            (
                                "idle",
                                format!("‹ {idle} ›{}", local("scenes.idle_minutes")),
                            )
                        }
                        SettingRow::Animations => {
                            let on = if app.config.animations { "on" } else { "off" };
                            ("animation", format!("‹ {on} ›{}", local("animations")))
                        }
                        SettingRow::Mouse => {
                            let on = if app.config.mouse {
                                "on: the wheel scrolls, a drag copies"
                            } else {
                                "off: the terminal's own"
                            };
                            ("mouse", format!("‹ {on} ›{}", local("mouse")))
                        }
                        SettingRow::StatusCpu
                        | SettingRow::StatusRam
                        | SettingRow::StatusBattery
                        | SettingRow::StatusClock => {
                            let s = app.config.status;
                            let (name, on, key) = match row {
                                SettingRow::StatusCpu => ("cpu", s.cpu, "status.cpu"),
                                SettingRow::StatusRam => ("ram", s.ram, "status.ram"),
                                SettingRow::StatusBattery => {
                                    ("battery", s.battery, "status.battery")
                                }
                                _ => ("clock", s.clock, "status.clock"),
                            };
                            let on = if on { "on, in the top right" } else { "off" };
                            (name, format!("‹ {on} ›{}", local(key)))
                        }
                        SettingRow::DiffLayout => {
                            let layout = match app.config.diff.layout {
                                DiffLayout::Unified => "unified",
                                DiffLayout::Split => "split: old and new side by side",
                            };
                            (
                                "diff layout",
                                format!("‹ {layout} ›{}", local("diff.layout")),
                            )
                        }
                        SettingRow::TeachAgents => {
                            let on = if app.config.agents.teach {
                                "told of termist spawn, worktree, open"
                            } else {
                                "off: not told of termist's commands"
                            };
                            ("agents", format!("‹ {on} ›{}", local("agents.teach")))
                        }
                        SettingRow::PullRequests => {
                            let on = if app.config.github.enabled {
                                "on: through gh"
                            } else {
                                "off"
                            };
                            (
                                "pull requests",
                                format!("‹ {on} ›{}", local("github.enabled")),
                            )
                        }
                    };
                    Line::from(Span::styled(
                        format!(" {name:<13} {value}"),
                        highlighted(Style::default(), i == view.row),
                    ))
                })
                .collect();
            let path = app
                .config_path
                .as_ref()
                .map_or("default settings".to_string(), |p| p.display().to_string());
            let mut extra = vec![
                Line::default(),
                Line::from(Span::styled(format!(" {path}"), dim())),
            ];
            if let Some(note) = &view.note {
                extra.push(Line::from(Span::styled(format!(" {note}"), t.warn)));
            }
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: "settings".into(),
                    width: 64,
                    query: None,
                    rows,
                    highlight: view.row,
                    extra,
                },
            );
        }
        Overlay::Keys(view) => {
            let prefix = app.keymap.prefix.to_string();
            let rows = key_rows()
                .into_iter()
                .enumerate()
                .map(|(i, (context, action))| {
                    let keys: Vec<String> = app
                        .keymap
                        .keys(context, action)
                        .iter()
                        .map(|k| match context {
                            Context::Grid => k.to_string(),
                            Context::Focus => format!("{prefix} {k}"),
                        })
                        .collect();
                    let keys = if keys.is_empty() {
                        "—".to_string()
                    } else {
                        keys.join(" ")
                    };
                    let place = match context {
                        Context::Grid => "grid",
                        Context::Focus => "focus",
                    };
                    Line::from(vec![
                        Span::styled(format!(" {place:<6}"), dim()),
                        Span::styled(
                            format!("{keys:<14} {}", action.label()),
                            highlighted(Style::default(), i == view.row),
                        ),
                    ])
                })
                .collect();
            let extra = view
                .note
                .iter()
                .map(|note| Line::from(Span::styled(format!(" {note}"), t.warn)))
                .collect();
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: "keys".into(),
                    width: 72,
                    query: None,
                    rows,
                    highlight: view.row,
                    extra,
                },
            );
        }
        Overlay::KeyCapture(capture) => {
            let what = match capture.target {
                CaptureTarget::Prefix => "press the new prefix".to_string(),
                CaptureTarget::Key(_, action) => {
                    format!("press the new key for: {}", action.label())
                }
            };
            let mut lines = vec![Line::from(format!(" {what}"))];
            if let Some(note) = &capture.note {
                lines.push(Line::from(Span::styled(format!(" {note}"), t.warn)));
            }
            let area = centered(body, 68, lines.len() as u16 + 2);
            boxed(f, t, area, "new key", lines);
        }
        Overlay::Worktrees(picker) => {
            let rows = picker
                .visible()
                .map(|(i, _, on)| {
                    let label = format!(" {}", picker.label(i));
                    Line::from(Span::styled(label, highlighted(Style::default(), on)))
                })
                .collect();
            let rows = if picker.items().is_empty() {
                vec![Line::from(Span::styled(
                    " no worktrees: Ctrl+N in the new-task prompt makes one",
                    dim(),
                ))]
            } else {
                rows
            };
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: "worktrees".into(),
                    width: 76,
                    query: None,
                    rows,
                    highlight: picker.highlight(),
                    extra: vec![],
                },
            );
        }
        Overlay::Target(picker) => {
            let rows = picker
                .visible()
                .map(|(i, _, on)| {
                    let label = format!(" {}", picker.label(i));
                    Line::from(Span::styled(label, highlighted(Style::default(), on)))
                })
                .collect();
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: "start the task in".into(),
                    width: 64,
                    query: picker.query().map(str::to_string),
                    rows,
                    highlight: picker.highlight(),
                    extra: vec![],
                },
            );
        }
        Overlay::Hand { pr, picker, .. } => {
            let rows = picker
                .visible()
                .map(|(i, _, on)| {
                    let label = format!(" {}", picker.label(i));
                    Line::from(Span::styled(label, highlighted(Style::default(), on)))
                })
                .collect();
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: format!("review comments of #{} to", pr.number),
                    width: 56,
                    query: None,
                    rows,
                    highlight: picker.highlight(),
                    extra: vec![],
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
                t,
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
        Overlay::Repos { project, picker } => {
            let name = app
                .state
                .projects
                .iter()
                .find(|p| p.id == *project)
                .map_or("", |p| p.name.as_str());
            // Why no account reads a repo when GitHub as a whole is the trouble.
            let everywhere = app.prs.get(project).map_or(&GhState::Ok, |d| &d.state);
            let rows = picker
                .visible()
                .map(|(_, r, on)| {
                    let mark = if r.visible { "[x]" } else { "[ ]" };
                    let count = r
                        .open_count
                        .map_or("—".to_string(), |n| format!("{n} open"));
                    let account = match (&r.state, &r.account) {
                        (GhState::NoAccess, _) => "no access".to_string(),
                        (GhState::LoggedOut, Some(a)) => format!("{a} logged out"),
                        (GhState::Failed(why), None) => crate::prs::markdown::cut(why, 22),
                        (GhState::Ok, None) => match everywhere {
                            GhState::Failed(why) => crate::prs::markdown::cut(why, 22),
                            // Access not asked yet.
                            GhState::Ok => "…".to_string(),
                            other => crate::prs::inbox_view::short_trouble(other)
                                .unwrap_or("…")
                                .to_string(),
                        },
                        (other, None) => crate::prs::inbox_view::short_trouble(other)
                            .unwrap_or("…")
                            .to_string(),
                        (_, Some(a)) if r.pinned => format!("{a}*"),
                        (_, Some(a)) => a.clone(),
                    };
                    let text = format!(
                        " {mark} {:<24} {count:>8}   {account}",
                        crate::prs::markdown::cut(&r.name, 24)
                    );
                    Line::from(Span::styled(text, highlighted(Style::default(), on)))
                })
                .collect();
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: format!("repos in {name}"),
                    width: 64,
                    query: None,
                    rows,
                    highlight: picker.highlight(),
                    extra: vec![],
                },
            );
        }
        Overlay::Compose(c) => compose_box(f, t, body, c, top),
        Overlay::RepoAccount { picker, .. } => {
            let rows = picker
                .visible()
                .map(|(_, a, on)| {
                    let text = match a {
                        None => " auto: the account with the most access".to_string(),
                        Some(login) => format!(" {login}"),
                    };
                    Line::from(Span::styled(text, highlighted(Style::default(), on)))
                })
                .collect();
            draw_list(
                f,
                t,
                body,
                ListBox {
                    title: "read this repo as".into(),
                    width: 48,
                    query: None,
                    rows,
                    highlight: picker.highlight(),
                    extra: vec![],
                },
            );
        }
    }
}

/// `text` with the characters at `marks` in `mark` and the rest in `base`.
fn marked(text: &str, marks: &[u32], base: Style, mark: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut on = false;
    for (i, c) in text.chars().enumerate() {
        let here = marks.binary_search(&(i as u32)).is_ok();
        if here != on && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if on { mark } else { base },
            ));
        }
        on = here;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if on { mark } else { base }));
    }
    spans
}

/// `f` and `F`: the query, the results with what matched, and what is going on.
fn draw_finder(f: &mut Frame, t: &Theme, body: Rect, finder: &crate::finder::Finder) {
    use crate::finder::{FindKind, MIN_QUERY};
    let folder = finder
        .folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let title = match finder.kind {
        FindKind::Files => {
            let n = finder.files.as_ref().map_or(0, |f| f.len()) as u32 + finder.more_files;
            let what = if n == 1 { "file" } else { "files" };
            format!("f · {folder} · {n} {what}")
        }
        FindKind::Grep => {
            let (n, more) = (finder.hits.len(), if finder.more { "+" } else { "" });
            let what = if n == 1 && more.is_empty() {
                "match"
            } else {
                "matches"
            };
            format!("F · {folder} · {n}{more} {what}")
        }
    };
    let mark = t.accent.add_modifier(Modifier::BOLD);
    let rows = finder
        .hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let on = i == finder.highlight;
            let base = highlighted(t, Style::default(), on);
            let mut spans = vec![Span::styled(" ", base)];
            match (&hit.line, &hit.text) {
                (Some(line), Some(text)) => {
                    spans.push(Span::styled(
                        format!("{}:{line}  ", hit.path),
                        highlighted(t, t.dim, on),
                    ));
                    spans.extend(marked(text.trim_end(), &hit.marks, base, mark.patch(base)));
                }
                _ => spans.extend(marked(&hit.path, &hit.marks, base, mark.patch(base))),
            }
            Line::from(spans)
        })
        .collect();
    let say = match (&finder.failed, finder.kind) {
        (Some(why), _) => Some(why.clone()),
        _ if finder.waiting && finder.hits.is_empty() => Some(match finder.kind {
            FindKind::Files => "reading the files…".to_string(),
            FindKind::Grep => "looking…".to_string(),
        }),
        (None, FindKind::Grep) if finder.query.chars().count() < MIN_QUERY => {
            Some(format!("type {MIN_QUERY} letters or more"))
        }
        (None, FindKind::Grep) if finder.hits.is_empty() && finder.due.is_none() => {
            Some("no matches".to_string())
        }
        (None, FindKind::Files) if finder.hits.is_empty() && finder.files.is_some() => {
            Some("no files match".to_string())
        }
        _ => None,
    };
    draw_list(
        f,
        t,
        body,
        ListBox {
            title,
            width: 84,
            query: Some(finder.query.clone()),
            rows,
            highlight: finder.highlight,
            extra: say
                .map(|s| vec![Line::from(Span::styled(format!(" {s}"), t.dim))])
                .unwrap_or_default(),
        },
    );
}

/// The footer line while `overlay` is on top.
pub fn hint(overlay: &Overlay) -> &'static str {
    match overlay {
        Overlay::Finder(_) => "type to find · Tab files/text · ↑/↓ choose · Enter open · Esc close",
        Overlay::Presets {
            deleting: Some(_), ..
        } => "y delete it · any key: keep it",
        Overlay::PresetName { .. } => "Enter keep this name · Esc cancel",
        Overlay::Presets { .. } => {
            "j/k choose · Enter new task with it · r rename · d delete · Esc close"
        }
        Overlay::Harness(_) => "j/k choose · Enter start · 1-3 pick · Esc cancel",
        Overlay::QuickPrompt(_) => {
            "Enter start · Alt+Enter newline · ↑ history · Tab CLI · ^O model · ^P project · ^S save preset · Esc cancel"
        }
        Overlay::Model(m) if m.efforts().is_empty() => {
            "type to filter · ↑↓ model · Enter choose · Esc back"
        }
        Overlay::Model(_) => "type to filter · ↑↓ model · ←→ effort · Enter choose · Esc back",
        Overlay::ModelName(_) => "Enter use this model · Esc back",
        Overlay::Project(_) => "type to filter · ↑/↓ choose · Enter pick · Esc back",
        Overlay::FollowUp { .. } => "Enter send to the agent · Alt+Enter newline · Esc cancel",
        Overlay::Hand { .. } => "↑/↓ choose · Enter there · Esc cancel",
        Overlay::Target(_) => "type to filter · ↑/↓ choose · Enter there · Esc back",
        Overlay::Worktrees(_) => "↑/↓ choose · Enter show or hide · X remove · Esc close",
        Overlay::Rename { .. } => "Enter rename · Esc cancel",
        Overlay::Palette(_) => "type to filter · ↑/↓ choose · Enter go there · Esc close",
        Overlay::OpenProject(_) => {
            "type to filter · Enter open · → in · ← up · Tab open this folder · Esc close"
        }
        Overlay::Help { .. } => "j/k scroll · Esc close",
        Overlay::Settings(_) => "j/k choose · ←/→ change · Enter set · Esc close",
        Overlay::Keys(_) => "j/k choose · Enter new key · Backspace no key · R default · Esc back",
        Overlay::KeyCapture(c) if c.conflict.is_some() => "Enter swap · Esc cancel",
        Overlay::KeyCapture(_) => "press a key · Esc cancel",
        Overlay::Repos { .. } => "j/k choose · Space show/hide · a account · Esc close",
        Overlay::RepoAccount { .. } => "j/k choose · Enter use · Esc back",
        Overlay::Compose(_) => "Enter send · Alt+Enter new line · Esc keep the draft",
    }
}

fn depth_name(depth: ColorDepth) -> &'static str {
    match depth {
        ColorDepth::Auto => "auto",
        ColorDepth::TrueColor => "24-bit",
        ColorDepth::Ansi256 => "256 colours",
        ColorDepth::Ansi16 => "16 colours",
    }
}

/// Every key: the grid's and focus mode's as bound now, then the fixed ones.
pub fn help_lines(app: &App) -> Vec<Line<'static>> {
    let t = &app.theme;
    let heading =
        |text: String| Line::from(Span::styled(text, t.accent.add_modifier(Modifier::BOLD)));
    let row = |key: String, what: &str| {
        Line::from(vec![
            Span::raw(format!("  {key:<12} ")),
            Span::styled(what.to_string(), t.dim),
        ])
    };
    let mut lines = vec![Line::from(format!(
        "termist {} · {}",
        env!("CARGO_PKG_VERSION"),
        app.config_path
            .as_ref()
            .map_or("default settings".to_string(), |p| p.display().to_string())
    ))];
    let prefix = app.keymap.prefix.to_string();
    for (context, title) in [
        (Context::Grid, "Grid".to_string()),
        (Context::Focus, format!("Focus mode, after {prefix}")),
    ] {
        lines.push(Line::default());
        lines.push(heading(title));
        let mut tabs_done = false;
        for action in keys::actions(context) {
            let keys = app.keymap.keys(context, *action);
            if let KeyAction::Tab(_) = action {
                // Nine tab keys read as one line.
                if !tabs_done {
                    tabs_done = true;
                    let tabs: Vec<String> = (1..=9)
                        .filter_map(|n| app.keymap.key(context, KeyAction::Tab(n)))
                        .collect();
                    let digits: Vec<String> = (1..=9).map(|n| n.to_string()).collect();
                    if tabs == digits {
                        lines.push(row("1-9".into(), "project tab 1 to 9"));
                    } else if !tabs.is_empty() {
                        lines.push(row(tabs.join(" "), "project tab 1 to 9"));
                    }
                }
                continue;
            }
            if keys.is_empty() {
                continue;
            }
            let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
            lines.push(row(keys.join(" "), action.label()));
        }
        if context == Context::Focus {
            lines.push(row(
                format!("{prefix} {prefix}"),
                "send the prefix to the session",
            ));
        }
    }
    lines.push(Line::default());
    lines.push(heading("Pull requests (v)".into()));
    for (key, what) in [
        (
            "Enter",
            "open the pull request; in a conversation, fold a thread",
        ),
        ("/", "search titles, numbers and authors"),
        ("f", "all · asked of you · yours"),
        ("m", "repos: show or hide, the account each is read with"),
        ("b", "open in the browser (a check's log on the checks tab)"),
        ("Tab", "next section: overview, conversation, checks, files"),
        ("n / N", "next / previous open thread"),
        ("d", "the files and their diff"),
        (
            "c",
            "comment on the pull request (in the diff: on the line)",
        ),
        ("r", "on a thread: reply"),
        ("x", "on a thread: resolve, or unresolve"),
        ("e / D", "on a comment of yours: edit / delete"),
        ("A", "send your review: comment, approve, request changes"),
        ("w", "a worktree on its branch, and an agent there"),
        ("Space", "on a thread: mark it for an agent"),
        ("a", "the marked threads (or this one) to an agent"),
        ("Esc", "back"),
    ] {
        lines.push(row(key.into(), what));
    }
    lines.push(Line::default());
    lines.push(heading("A pull request's diff (d)".into()));
    for (key, what) in [
        ("Tab", "the file tree or the diff"),
        ("j / k", "the line cursor; v starts a range"),
        ("c", "comment on the line or the range, into your review"),
        ("C-s", "in a line comment: the lines as a suggestion"),
        ("J / K", "next / previous file"),
        ("{ / }", "previous / next hunk"),
        ("n / N", "next / previous thread"),
        ("Enter", "open a file, fold a folder or a thread"),
        (
            "C-r",
            "mark the file viewed on GitHub, or not; then the next one",
        ),
        ("s", "unified or split"),
        ("← →", "move long lines sideways"),
        ("/", "search the paths"),
        ("b", "the file on GitHub"),
        ("Space / a", "mark the thread / the marked ones to an agent"),
        ("Esc", "back to the pull request"),
    ] {
        lines.push(row(key.into(), what));
    }
    lines.push(Line::default());
    lines.push(heading("Mouse".into()));
    for (key, what) in [
        ("click", "a project tab, a card, a panel, a file, a thread"),
        ("click again", "the selected card or pull request: open it"),
        ("click pane", "type into the session, as Enter"),
        ("wheel", "moves what is under it"),
    ] {
        lines.push(row(key.into(), what));
    }
    lines.push(Line::default());
    lines.push(heading("Anywhere".into()));
    lines.push(row("C-q".into(), "out of anything, back to the grid"));
    lines.push(row("C-c".into(), "quit"));
    lines.push(Line::default());
    lines.push(heading("Text boxes".into()));
    for (key, what) in [
        ("Enter", "send"),
        ("Alt+Enter", "new line (also Shift+Enter, C-j)"),
        ("↑ ↓", "earlier prompts, or lines"),
        ("C-a C-e", "start, end of the line"),
        ("C-u C-k", "delete to the start, to the end"),
        ("Alt+← →", "a word back, forward"),
        ("Esc", "close"),
    ] {
        lines.push(row(key.into(), what));
    }
    lines.push(Line::default());
    lines.push(heading("Scrolling back".into()));
    for (key, what) in [
        ("wheel", "over the pane: its history"),
        ("drag", "over the pane: selects and copies"),
        ("↑ ↓ j k", "a line"),
        ("PgUp PgDn", "a page (also C-b, C-f)"),
        ("C-u C-d", "half a page"),
        ("g", "the oldest line"),
        ("q Esc G", "back to the live screen"),
    ] {
        lines.push(row(key.into(), what));
    }
    lines.push(Line::default());
    lines.push(heading("Lists".into()));
    for (key, what) in [
        ("letters", "filter, where the list can be typed into"),
        ("↑ ↓ C-n C-p", "choose"),
        ("j k", "choose, where the list cannot be typed into"),
        ("Enter", "pick"),
        ("Esc", "back"),
    ] {
        lines.push(row(key.into(), what));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Grid and focus keys change in config.toml: [keys.grid] and [keys.focus]",
        t.dim,
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lines_wrap_and_the_cursor_follows() {
        let input = TextInput::with_text("abcdefg\nxy", true);
        let (rows, cursor) = wrapped_rows(&input, 3);
        assert_eq!(rows, vec!["abc", "def", "g", "xy"]);
        assert_eq!(cursor, (3, 2));
    }

    #[test]
    fn lines_break_after_a_space_when_they_have_one() {
        let input = TextInput::with_text("fix the login", true);
        let (rows, cursor) = wrapped_rows(&input, 6);
        assert_eq!(rows, vec!["fix ", "the ", "login"]);
        assert_eq!(cursor, (2, 5));
        let mut input = TextInput::with_text("fix the login", true);
        for _ in 0..9 {
            input.key(ratatui::crossterm::event::KeyEvent::from(
                ratatui::crossterm::event::KeyCode::Left,
            ));
        }
        assert_eq!(
            wrapped_rows(&input, 6).1,
            (1, 0),
            "after the space: next row"
        );
    }

    #[test]
    fn a_line_as_wide_as_the_box_leaves_room_for_the_cursor() {
        let input = TextInput::with_text("abc", true);
        assert_eq!(
            wrapped_rows(&input, 3),
            (vec!["abc".to_string(), String::new()], (1, 0))
        );
        let empty = TextInput::new(true);
        assert_eq!(wrapped_rows(&empty, 3), (vec![String::new()], (0, 0)));
    }
}
