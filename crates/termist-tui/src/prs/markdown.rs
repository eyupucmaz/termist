//! Markdown from GitHub (PR descriptions, comments) as lines that fit a width. A
//! subset: headings, paragraphs, lists, quotes, code, links, images as
//! `[image: alt]`. HTML is dropped: PR templates are full of comments.
//!
//! The text is anyone's who can comment on a pull request, and it is drawn every
//! frame: nesting shows at most a few levels deep, a prefix never takes more than
//! half the line, and what was rendered is kept for the next frame.
use crate::theme::Theme;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Quote bars shown at most; deeper quotes keep this many.
const MAX_QUOTES: usize = 4;
/// List indent levels shown at most.
const MAX_INDENT: usize = 4;
/// Rendered texts kept, and the lines they may hold together.
const CACHE_ENTRIES: usize = 4096;
const CACHE_LINES: usize = 100_000;

pub fn width_of(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// `text` cut to `width` columns, ending in `…` when cut.
pub fn cut(text: &str, width: usize) -> String {
    if width_of(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// `<!-- … -->` taken out; one left open runs to the end.
pub fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("-->") {
            Some(end) => &rest[start + end + 3..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// What a text was rendered as, for a width and the theme's two styles.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    text: u64,
    len: usize,
    width: u16,
    dim: Style,
    accent: Style,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, (Vec<Line<'static>>, u64)>,
    lines: usize,
    clock: u64,
}

impl Cache {
    fn get(&mut self, key: &Key) -> Option<Vec<Line<'static>>> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|(lines, used)| {
            *used = clock;
            lines.clone()
        })
    }

    fn put(&mut self, key: Key, lines: Vec<Line<'static>>) {
        if self.entries.len() >= CACHE_ENTRIES || self.lines + lines.len() > CACHE_LINES {
            // The least recently used half goes.
            let mut by_use: Vec<(u64, Key)> = self
                .entries
                .iter()
                .map(|(k, (_, used))| (*used, *k))
                .collect();
            by_use.sort_unstable_by_key(|(used, _)| *used);
            for (_, k) in by_use.iter().take(by_use.len().div_ceil(2)) {
                if let Some((gone, _)) = self.entries.remove(k) {
                    self.lines -= gone.len();
                }
            }
        }
        self.lines += lines.len();
        self.entries.insert(key, (lines, self.clock));
    }
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// `text` as lines `width` wide; the same text at the same width is rendered once.
pub fn render(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let key = Key {
        text: hasher.finish(),
        len: text.len(),
        width,
        dim: theme.dim,
        accent: theme.accent,
    };
    if let Some(lines) = CACHE.with(|c| c.borrow_mut().get(&key)) {
        return lines;
    }
    let lines = render_now(text, width, theme);
    CACHE.with(|c| c.borrow_mut().put(key, lines.clone()));
    lines
}

fn render_now(text: &str, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    let text = strip_comments(text);
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut r = Renderer {
        width: (width as usize).max(8),
        theme,
        lines: vec![],
        pieces: vec![],
        styles: vec![],
        lists: vec![],
        item: None,
        quote: 0,
        code: false,
        image: None,
        link: None,
    };
    for event in Parser::new_ext(&text, options) {
        r.event(event);
    }
    r.flush();
    while r.lines.last().is_some_and(|l| l.width() == 0) {
        r.lines.pop();
    }
    r.lines
}

struct Renderer<'t> {
    width: usize,
    theme: &'t Theme,
    lines: Vec<Line<'static>>,
    /// The block being written: text and its style, not yet wrapped.
    pieces: Vec<(String, Style)>,
    styles: Vec<Style>,
    /// Open lists: the next number of an ordered one.
    lists: Vec<Option<u64>>,
    /// An item's marker, for its first line.
    item: Option<String>,
    quote: usize,
    code: bool,
    image: Option<String>,
    link: Option<String>,
}

impl Renderer<'_> {
    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or_default()
    }

    fn push(&mut self, text: &str, style: Style) {
        self.pieces.push((text.to_string(), style));
    }

    fn blank(&mut self) {
        if self.lines.last().is_some_and(|l| l.width() > 0) {
            self.lines.push(Line::default());
        }
    }

    /// The quote bars before a line, a few at most.
    fn bars(&self) -> String {
        "┃ ".repeat(self.quote.min(MAX_QUOTES))
    }

    /// A prefix wider than half the line leaves too little room for the text: a
    /// two-column one stands in for it.
    fn fit(&self, first: String, rest: String) -> (String, String) {
        if width_of(&first).max(width_of(&rest)) <= self.width / 2 {
            return (first, rest);
        }
        let short = if self.quote > 0 { "┃ " } else { "  " };
        (short.to_string(), short.to_string())
    }

    /// The quote bars and list indent before a line: the first line's and the rest's.
    fn prefixes(&mut self) -> (String, String) {
        let quote = self.bars();
        let depth = self.lists.len().saturating_sub(1).min(MAX_INDENT);
        let indent = "  ".repeat(depth);
        let (first, rest) = match self.item.take() {
            Some(marker) => {
                let pad = " ".repeat(width_of(&marker));
                (
                    format!("{quote}{indent}{marker}"),
                    format!("{quote}{indent}{pad}"),
                )
            }
            None if !self.lists.is_empty() => {
                let pad = format!("{quote}{indent}  ");
                (pad.clone(), pad)
            }
            None => (quote.clone(), quote),
        };
        self.fit(first, rest)
    }

    /// Wraps the block written so far into lines.
    fn flush(&mut self) {
        if self.pieces.iter().all(|(s, _)| s.trim().is_empty()) {
            self.pieces.clear();
            return;
        }
        let (first, rest) = self.prefixes();
        let dim = self.theme.dim;
        let mut filler = Filler::new(
            self.width,
            Span::styled(first, dim),
            Span::styled(rest, dim),
        );
        for word in words(&std::mem::take(&mut self.pieces)) {
            filler.word(word);
        }
        self.lines.extend(filler.finish());
    }

    fn code_text(&mut self, text: &str) {
        let (quote, _) = self.fit(self.bars(), String::new());
        let room = self.width.saturating_sub(width_of(&quote) + 2);
        for line in text.lines() {
            self.lines.push(Line::from(vec![
                Span::styled(quote.clone(), self.theme.dim),
                Span::raw("  "),
                Span::styled(cut(line, room), self.theme.dim),
            ]));
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if let Some(alt) = &mut self.image {
                    alt.push_str(&t);
                } else if self.code {
                    self.code_text(&t);
                } else {
                    let style = self.style();
                    self.push(&t, style);
                }
            }
            Event::Code(t) => {
                let style = self.theme.dim;
                self.push(&t, style);
            }
            Event::SoftBreak => self.push(" ", Style::default()),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                let rule = "─".repeat(self.width.min(30));
                self.lines
                    .push(Line::from(Span::styled(rule, self.theme.dim)));
                self.blank();
            }
            Event::TaskListMarker(done) => {
                let style = self.theme.dim;
                self.push(if done { "[x] " } else { "[ ] " }, style);
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        let style = self.style();
        match tag {
            Tag::Heading { .. } => {
                self.flush();
                self.styles
                    .push(self.theme.accent.add_modifier(Modifier::BOLD));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = true;
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                self.item = Some(match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}. ");
                        *n += 1;
                        marker
                    }
                    _ => "• ".to_string(),
                });
            }
            Tag::Emphasis => self.styles.push(style.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.styles.push(style.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.styles.push(style.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                // An autolink shows its address already.
                self.link = (link_type != LinkType::Autolink).then(|| dest_url.to_string());
                self.styles.push(style.add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { .. } => self.image = Some(String::new()),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Heading(_) => {
                self.styles.pop();
                self.flush();
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
                if self.quote == 0 {
                    self.blank();
                }
            }
            TagEnd::CodeBlock => {
                self.code = false;
                self.blank();
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some(host) = self.link.take().as_deref().and_then(host) {
                    let style = self.theme.dim;
                    self.push(&format!(" ({host})"), style);
                }
            }
            TagEnd::Image => {
                if let Some(alt) = self.image.take() {
                    let shown = if alt.is_empty() {
                        "[image]".to_string()
                    } else {
                        format!("[image: {alt}]")
                    };
                    let style = self.theme.dim;
                    self.push(&shown, style);
                }
            }
            TagEnd::TableCell => {
                let style = self.theme.dim;
                self.push(" │ ", style);
            }
            TagEnd::TableHead | TagEnd::TableRow => self.flush(),
            TagEnd::Table => self.blank(),
            _ => {}
        }
    }
}

/// `docs.rs` of `https://docs.rs/x`; nothing for a link inside the page.
fn host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())
}

/// The pieces as words: split at whitespace, a word keeping each part's style.
fn words(pieces: &[(String, Style)]) -> Vec<Vec<(String, Style)>> {
    let mut words = Vec::new();
    let mut word: Vec<(String, Style)> = Vec::new();
    for (text, style) in pieces {
        for c in text.chars() {
            if c.is_whitespace() {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                continue;
            }
            match word.last_mut() {
                Some((s, st)) if st == style => s.push(c),
                _ => word.push((c.to_string(), *style)),
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Fills lines up to a width, each starting with a prefix.
struct Filler {
    width: usize,
    rest: Span<'static>,
    lines: Vec<Line<'static>>,
    line: Vec<Span<'static>>,
    used: usize,
    /// Only the prefix on the line so far.
    bare: bool,
}

impl Filler {
    fn new(width: usize, first: Span<'static>, rest: Span<'static>) -> Filler {
        let used = first.width();
        Filler {
            width,
            rest,
            lines: vec![],
            line: vec![first],
            used,
            bare: true,
        }
    }

    fn newline(&mut self) {
        let line = std::mem::replace(&mut self.line, vec![self.rest.clone()]);
        self.lines.push(Line::from(line));
        self.used = self.rest.width();
        self.bare = true;
    }

    fn word(&mut self, word: Vec<(String, Style)>) {
        let wide: usize = word.iter().map(|(s, _)| width_of(s)).sum();
        if !self.bare {
            if self.used + 1 + wide > self.width {
                self.newline();
            } else {
                self.line.push(Span::raw(" "));
                self.used += 1;
            }
        }
        for (text, style) in word {
            let mut chunk = String::new();
            for c in text.chars() {
                let w = c.width().unwrap_or(0);
                // A word longer than the line is cut where the line ends.
                if self.used + w > self.width && !(self.bare && chunk.is_empty()) {
                    if !chunk.is_empty() {
                        self.line
                            .push(Span::styled(std::mem::take(&mut chunk), style));
                    }
                    self.newline();
                }
                chunk.push(c);
                self.used += w;
                self.bare = false;
            }
            if !chunk.is_empty() {
                self.line.push(Span::styled(chunk, style));
            }
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.bare {
            self.lines.push(Line::from(self.line));
        }
        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn md(src: &str, width: u16) -> Vec<String> {
        text(&render(src, width, &Theme::terminal()))
    }

    #[test]
    fn paragraphs_wrap_at_the_width() {
        assert_eq!(
            md("one two three four five\n\nsix", 10),
            ["one two", "three four", "five", "", "six"]
        );
    }

    #[test]
    fn headings_lists_and_quotes() {
        assert_eq!(
            md("# Title\n\n- a\n- b\n  1. x\n\n> quoted", 40),
            ["Title", "", "• a", "• b", "  1. x", "", "┃ quoted"]
        );
    }

    #[test]
    fn html_comments_and_blocks_are_dropped() {
        let src = "<!-- Describe your change -->\nHello <b>bold</b>\n\n<details>\n<summary>x</summary>\n</details>\n\nBye";
        assert_eq!(md(src, 40), ["Hello bold", "", "Bye"]);
        assert_eq!(strip_comments("a <!-- b"), "a ");
    }

    #[test]
    fn code_blocks_keep_their_lines_cut_to_the_width() {
        assert_eq!(
            md("```\nlet x = 1;\nlet a_very_long_line = 2;\n```", 16),
            ["  let x = 1;", "  let a_very_lo…"]
        );
    }

    #[test]
    fn links_show_their_host_and_images_their_alt() {
        assert_eq!(
            md(
                "See [the docs](https://docs.rs/x) and ![shot](https://x.png) <https://a.com>",
                80
            ),
            ["See the docs (docs.rs) and [image: shot] https://a.com"]
        );
    }

    #[test]
    fn wide_characters_wrap_by_their_width() {
        assert_eq!(
            md("日本語 日本語 日本語", 8),
            ["日本語", "日本語", "日本語"]
        );
        assert_eq!(
            md(&"x".repeat(25), 10),
            ["xxxxxxxxxx", "xxxxxxxxxx", "xxxxx"]
        );
        assert_eq!(cut("日本語", 5), "日本…");
        assert_eq!(cut("short", 10), "short");
    }

    /// Lines and bytes of what `src` renders to.
    fn size(src: &str, width: u16) -> (usize, usize) {
        let lines = md(src, width);
        (lines.len(), lines.iter().map(String::len).sum())
    }

    #[test]
    fn deep_nesting_shows_a_few_levels_and_never_eats_the_line() {
        let word = "x".repeat(2000);
        let (lines, bytes) = size(&format!("{}{word}", ">".repeat(10_000)), 80);
        assert!(lines <= 2000 / 40 + 3, "{lines} lines");
        assert!(bytes < 2000 * 2, "{bytes} bytes");
        assert_eq!(md(&format!("{}deep", ">".repeat(10)), 40), ["┃ ┃ ┃ ┃ deep"]);
        // Lists nest as deep as they like; the indent stops at four levels.
        let list: String = (0..3000)
            .map(|i| format!("{}- {i}\n", "  ".repeat(i)))
            .collect();
        let (lines, bytes) = size(&format!("{list}{}", "  ".repeat(3000) + &word), 80);
        assert!(lines <= 3000 + 2000 / 40 + 3, "{lines} lines");
        assert!(bytes < 3000 * 40 + 2000 * 2, "{bytes} bytes");
        assert_eq!(
            md(
                "- a\n  - b\n    - c\n      - d\n        - e\n          - f",
                40
            )[5],
            "        • f"
        );
    }

    #[test]
    fn a_prefix_takes_at_most_half_the_line() {
        // Four bars are eight columns: too many for a ten-column line.
        assert_eq!(
            md(&format!("{}abcdefghij", ">".repeat(6)), 10),
            ["┃ abcdefgh", "┃ ij"]
        );
        assert_eq!(
            md("> > > >\n> > > > ```\n> > > > code\n> > > > ```", 10),
            ["┃   code"]
        );
    }

    #[test]
    fn the_same_text_renders_the_same_from_the_cache() {
        let src = "> quoted **bold** text that wraps around";
        let first = render(src, 12, &Theme::terminal());
        assert_eq!(render(src, 12, &Theme::terminal()), first);
        assert_ne!(render(src, 20, &Theme::terminal()), first, "by width");
        let mut cache = Cache::default();
        for i in 0..CACHE_ENTRIES + 1 {
            let key = Key {
                text: i as u64,
                len: 1,
                width: 10,
                dim: Style::default(),
                accent: Style::default(),
            };
            cache.put(key, vec![Line::raw("x")]);
        }
        assert!(cache.entries.len() <= CACHE_ENTRIES);
        assert_eq!(cache.lines, cache.entries.len());
    }

    #[test]
    fn tasks_tables_and_rules_read_as_text() {
        assert_eq!(
            md(
                "- [x] done\n- [ ] todo\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\nend",
                30
            ),
            [
                "• [x] done",
                "• [ ] todo",
                "",
                "a │ b │",
                "1 │ 2 │",
                "",
                "──────────────────────────────",
                "",
                "end"
            ]
        );
    }
}
