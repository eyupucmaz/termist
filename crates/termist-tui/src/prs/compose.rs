//! The box a comment, a reply or a review is written in: what it goes to, its text,
//! and whether it is on its way. The app keeps drafts by target, so a box closed with
//! Esc opens again on the same words.
use crate::text_input::{Edit, TextInput};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use termist_core::github::{CommentKind, PrRef, PrWrite, Side, Verdict};

/// What the text goes to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// The pull request itself.
    Comment,
    /// A thread; `to` is who started it, for the title.
    Reply { thread: String, to: String },
    /// Lines of a file, into your pending review.
    Line {
        path: String,
        side: Side,
        line: u32,
        start: Option<u32>,
    },
    /// One of your comments, again.
    Edit { comment: String, kind: CommentKind },
    /// Your review.
    Submit,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Sending {
    #[default]
    Idle,
    /// Asked of GitHub under this ticket.
    Sending(u64),
    /// GitHub said no: the text stays for another try.
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compose {
    pub pr: PrRef,
    pub target: Target,
    pub input: TextInput,
    pub state: Sending,
    /// The lines a line comment is on, shown above the text: number, mark, text.
    pub context: Vec<(u32, char, String)>,
    /// Their new text, for a suggestion; empty when there is none to make.
    pub suggest: Vec<String>,
    /// A review's verdict.
    pub verdict: Verdict,
    /// Your own pull request: a review can only comment.
    pub mine: bool,
    /// Comments waiting in your review.
    pub pending: usize,
    /// A word under the text after a key that did nothing.
    pub note: Option<&'static str>,
}

/// What a key in the box asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeAction {
    None,
    /// Close, keeping the text as the target's draft.
    Close,
    Send(PrWrite),
}

/// `lines` as a block GitHub offers to apply in place of the lines commented on.
pub fn suggestion(lines: &[String]) -> String {
    format!("```suggestion\n{}\n```\n", lines.join("\n"))
}

impl Compose {
    pub fn new(pr: PrRef, target: Target, text: &str) -> Compose {
        Compose {
            pr,
            target,
            input: TextInput::with_text(text, true),
            state: Sending::Idle,
            context: vec![],
            suggest: vec![],
            verdict: Verdict::Comment,
            mine: false,
            pending: 0,
            note: None,
        }
    }

    pub fn title(&self) -> String {
        match &self.target {
            Target::Comment => format!("comment on #{}", self.pr.number),
            Target::Reply { to, .. } => format!("reply to {to}"),
            Target::Line {
                path, line, start, ..
            } => {
                let name = path.rsplit('/').next().unwrap_or(path);
                match start {
                    Some(s) => format!("comment on {name}:{s}–{line} · to your review"),
                    None => format!("comment on {name}:{line} · to your review"),
                }
            }
            Target::Edit { .. } => "edit your comment".into(),
            Target::Submit => {
                let plural = if self.pending == 1 { "" } else { "s" };
                format!(
                    "review #{} · {} comment{plural} pending",
                    self.pr.number, self.pending
                )
            }
        }
    }

    /// The write the text makes, or why it cannot go as it is.
    pub fn write(&self) -> Result<PrWrite, &'static str> {
        let body = self.input.text().trim_end().to_string();
        let empty = body.trim().is_empty();
        if empty && self.target != Target::Submit {
            return Err("write something first");
        }
        Ok(match &self.target {
            Target::Comment => PrWrite::Comment { body },
            Target::Reply { thread, .. } => PrWrite::Reply {
                thread: thread.clone(),
                body,
            },
            Target::Line {
                path,
                side,
                line,
                start,
            } => PrWrite::LineComment {
                path: path.clone(),
                side: *side,
                line: *line,
                start: *start,
                body,
            },
            Target::Edit { comment, kind } => PrWrite::Edit {
                comment: comment.clone(),
                kind: *kind,
                body,
            },
            Target::Submit => {
                // GitHub takes neither without a word.
                match self.verdict {
                    Verdict::RequestChanges if empty => return Err("say what to change"),
                    Verdict::Comment if empty && self.pending == 0 => {
                        return Err("say something, or add comments first");
                    }
                    _ => {}
                }
                PrWrite::Submit {
                    verdict: self.verdict,
                    body,
                }
            }
        })
    }

    pub fn key(&mut self, key: KeyEvent) -> ComposeAction {
        self.note = None;
        if key.code == KeyCode::Esc {
            return ComposeAction::Close;
        }
        if matches!(self.state, Sending::Sending(_)) {
            return ComposeAction::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if ctrl => {
                if self.suggest.is_empty() {
                    self.note = Some("a suggestion needs new lines");
                } else {
                    self.input.insert_str(&suggestion(&self.suggest));
                }
                return ComposeAction::None;
            }
            KeyCode::Tab | KeyCode::BackTab if self.target == Target::Submit => {
                let all: &[Verdict] = if self.mine {
                    &[Verdict::Comment]
                } else {
                    &Verdict::ALL
                };
                let at = all.iter().position(|v| *v == self.verdict).unwrap_or(0) as isize;
                let step = if key.code == KeyCode::Tab { 1 } else { -1 };
                self.verdict = all[(at + step).rem_euclid(all.len() as isize) as usize];
                if self.mine {
                    self.note = Some("on your own pull request a review can only comment");
                }
                return ComposeAction::None;
            }
            _ => {}
        }
        if self.input.key(key) != Edit::Submit {
            return ComposeAction::None;
        }
        match self.write() {
            Ok(write) => ComposeAction::Send(write),
            Err(why) => {
                self.note = Some(why);
                ComposeAction::None
            }
        }
    }

    /// The line under the text: what is happening, or the keys.
    pub fn footer(&self) -> String {
        match (&self.state, self.note) {
            (Sending::Sending(_), _) => "sending…".into(),
            (Sending::Failed(why), _) => format!("couldn't post · {why}"),
            (_, Some(note)) => note.into(),
            _ => {
                let send = if matches!(self.target, Target::Line { .. }) {
                    "Enter add"
                } else {
                    "Enter send"
                };
                let extra = match self.target {
                    Target::Line { .. } => " · C-s suggestion",
                    Target::Submit => " · Tab verdict",
                    _ => "",
                };
                format!("{send} · Alt+Enter new line{extra} · Esc keep")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode as K, KeyModifiers as M};
    use termist_core::github::RepoId;

    fn pr() -> PrRef {
        PrRef {
            repo: RepoId(1),
            number: 212,
        }
    }

    fn line() -> Target {
        Target::Line {
            path: "src/search/DealerFilter.tsx".into(),
            side: Side::Right,
            line: 42,
            start: Some(40),
        }
    }

    fn typed(c: &mut Compose, s: &str) {
        for ch in s.chars() {
            c.key(KeyEvent::new(K::Char(ch), M::NONE));
        }
    }

    fn enter(c: &mut Compose) -> ComposeAction {
        c.key(KeyEvent::new(K::Enter, M::NONE))
    }

    #[test]
    fn titles_say_where_the_text_goes() {
        assert_eq!(
            Compose::new(pr(), Target::Comment, "").title(),
            "comment on #212"
        );
        assert_eq!(
            Compose::new(pr(), line(), "").title(),
            "comment on DealerFilter.tsx:40–42 · to your review"
        );
        let reply = Target::Reply {
            thread: "T1".into(),
            to: "carol".into(),
        };
        assert_eq!(Compose::new(pr(), reply, "").title(), "reply to carol");
        let mut review = Compose::new(pr(), Target::Submit, "");
        review.pending = 3;
        assert_eq!(review.title(), "review #212 · 3 comments pending");
    }

    #[test]
    fn enter_sends_what_was_written_and_empty_text_waits() {
        let mut c = Compose::new(pr(), line(), "");
        assert_eq!(enter(&mut c), ComposeAction::None);
        assert_eq!(c.note, Some("write something first"));
        typed(&mut c, "memoize?");
        assert_eq!(
            enter(&mut c),
            ComposeAction::Send(PrWrite::LineComment {
                path: "src/search/DealerFilter.tsx".into(),
                side: Side::Right,
                line: 42,
                start: Some(40),
                body: "memoize?".into(),
            })
        );
        c.state = Sending::Sending(1);
        typed(&mut c, "x");
        assert_eq!(
            c.input.text(),
            "memoize?",
            "no typing while it is on its way"
        );
        assert_eq!(c.key(KeyEvent::new(K::Esc, M::NONE)), ComposeAction::Close);
    }

    #[test]
    fn a_suggestion_puts_the_new_lines_in_a_block() {
        let mut c = Compose::new(pr(), line(), "Like this:\n");
        c.key(KeyEvent::new(K::Char('s'), M::CONTROL));
        assert_eq!(c.note, Some("a suggestion needs new lines"));
        c.suggest = vec!["const a = 1;".into(), "  return a;".into()];
        c.key(KeyEvent::new(K::Char('s'), M::CONTROL));
        assert_eq!(
            c.input.text(),
            "Like this:\n```suggestion\nconst a = 1;\n  return a;\n```\n"
        );
    }

    #[test]
    fn a_review_takes_a_verdict_with_github_s_rules() {
        let mut c = Compose::new(pr(), Target::Submit, "");
        assert_eq!(enter(&mut c), ComposeAction::None);
        assert_eq!(c.note, Some("say something, or add comments first"));
        c.pending = 2;
        assert!(matches!(
            enter(&mut c),
            ComposeAction::Send(PrWrite::Submit {
                verdict: Verdict::Comment,
                ..
            })
        ));
        c.key(KeyEvent::new(K::Tab, M::NONE));
        assert_eq!(c.verdict, Verdict::Approve);
        assert!(matches!(
            enter(&mut c),
            ComposeAction::Send(PrWrite::Submit {
                verdict: Verdict::Approve,
                ..
            })
        ));
        c.key(KeyEvent::new(K::Tab, M::NONE));
        assert_eq!(enter(&mut c), ComposeAction::None);
        assert_eq!(c.note, Some("say what to change"));
        c.key(KeyEvent::new(K::BackTab, M::NONE));
        assert_eq!(c.verdict, Verdict::Approve);
        let mut own = Compose::new(pr(), Target::Submit, "");
        own.mine = true;
        own.key(KeyEvent::new(K::Tab, M::NONE));
        assert_eq!(own.verdict, Verdict::Comment, "your own pull request");
        assert_eq!(
            own.note,
            Some("on your own pull request a review can only comment")
        );
    }

    #[test]
    fn the_footer_tells_what_is_happening() {
        let mut c = Compose::new(pr(), line(), "a");
        assert_eq!(
            c.footer(),
            "Enter add · Alt+Enter new line · C-s suggestion · Esc keep"
        );
        c.state = Sending::Sending(3);
        assert_eq!(c.footer(), "sending…");
        c.state = Sending::Failed("no access".into());
        assert_eq!(c.footer(), "couldn't post · no access");
    }
}
