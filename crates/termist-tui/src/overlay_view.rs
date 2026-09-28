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
use termist_core::config::{ColorDepth, PanePosition, Sounds};

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
    f.render_widget(Clear, area);
    f.buffer_mut().set_style(area, theme.base);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
        ),
        area,
    );
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
            let height = q.input.text().split('\n').count().clamp(3, 8);
            let area = centered(body, 72, height as u16 + 3);
            let inner_w = area.width.saturating_sub(2);
            let rows = area.height.saturating_sub(3) as usize;
            let (mut lines, (cx, cy)) = input_view(&q.input, inner_w, rows);
            lines.resize(rows, Line::default());
            lines.push(Line::from(Span::styled(launch_line(app, q), dim())));
            boxed(f, t, area, "new task", lines);
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
                t,
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
        Overlay::ModelName(input) => text_box(f, t, body, "model name", 48, input, top),
        Overlay::FollowUp { session, input } => {
            let name = app
                .state
                .sessions
                .iter()
                .find(|s| s.id == *session)
                .map_or("?", |s| s.display_name());
            text_box(f, t, body, &format!("follow-up · {name}"), 64, input, top);
        }
        Overlay::Rename { input, .. } => text_box(f, t, body, "rename", 48, input, top),
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
                                Theme::name_of(&app.config.theme),
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
                                PanePosition::Bottom => "under the cards",
                                PanePosition::Right => "right of the cards",
                            };
                            ("pane", format!("‹ {place} ›{}", local("pane_position")))
                        }
                        SettingRow::Keys => ("keys", format!("…{}", local("keys"))),
                        SettingRow::Sounds => {
                            let sounds = match app.config.notify.sounds {
                                Sounds::Istanbul => "istanbul (vapur waits, martı done)",
                                Sounds::System => "the system's",
                                Sounds::Bell => "the terminal bell",
                                Sounds::Off => "off",
                            };
                            ("sounds", format!("‹ {sounds} ›{}", local("notify.sounds")))
                        }
                        SettingRow::Desktop => {
                            let on = if app.config.notify.desktop {
                                "on, when the terminal is not in front"
                            } else {
                                "off"
                            };
                            ("desktop", format!("‹ {on} ›{}", local("notify.desktop")))
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
                    };
                    Line::from(Span::styled(
                        format!(" {name:<9} {value}"),
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
    }
}

/// The footer line while `overlay` is on top.
pub fn hint(overlay: &Overlay) -> &'static str {
    match overlay {
        Overlay::Harness(_) => "j/k choose · Enter start · 1-3 pick · Esc cancel",
        Overlay::QuickPrompt(_) => {
            "Enter start · Alt+Enter newline · ↑ history · Tab CLI · ^O model · ^P project · Esc cancel"
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
        Overlay::Help { .. } => "j/k scroll · Esc close",
        Overlay::Settings(_) => "j/k choose · ←/→ change · Enter set · Esc close",
        Overlay::Keys(_) => "j/k choose · Enter new key · Backspace no key · R default · Esc back",
        Overlay::KeyCapture(c) if c.conflict.is_some() => "Enter swap · Esc cancel",
        Overlay::KeyCapture(_) => "press a key · Esc cancel",
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
