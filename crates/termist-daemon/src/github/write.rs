//! Writing to a pull request through GraphQL mutations: comments, replies, line
//! comments, reviews, resolving threads, editing and deleting your comments.
//!
//! GitHub opens your pending review itself when a line comment comes without one, and
//! puts a reply into it while you have one, so only sending a review names it.
use super::gh::{self, Gh};
use super::query::quoted;
use serde_json::Value;
use termist_core::github::{CommentKind, GhState, PrWrite, Side, Verdict};

fn side(s: Side) -> &'static str {
    match s {
        Side::Left => "LEFT",
        Side::Right => "RIGHT",
    }
}

fn event(v: Verdict) -> &'static str {
    match v {
        Verdict::Comment => "COMMENT",
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
    }
}

/// The mutation that does `write` on pull request `pr` (its node id), and the field its
/// answer comes in. `pending` is your pending review, if you have one.
pub fn mutation(write: &PrWrite, pr: &str, pending: Option<&str>) -> (String, &'static str) {
    let (field, input) = match write {
        PrWrite::Comment { body } => (
            "addComment",
            format!("subjectId: {}, body: {}", quoted(pr), quoted(body)),
        ),
        PrWrite::Reply { thread, body } => (
            "addPullRequestReviewThreadReply",
            format!(
                "pullRequestReviewThreadId: {}, body: {}",
                quoted(thread),
                quoted(body)
            ),
        ),
        PrWrite::LineComment {
            path,
            side: s,
            line,
            start,
            body,
        } => {
            let start = start
                .map(|n| format!(", startLine: {n}, startSide: {}", side(*s)))
                .unwrap_or_default();
            (
                "addPullRequestReviewThread",
                format!(
                    "pullRequestId: {}, path: {}, line: {line}, side: {}{start}, body: {}",
                    quoted(pr),
                    quoted(path),
                    side(*s),
                    quoted(body)
                ),
            )
        }
        PrWrite::Submit { verdict, body } => {
            let body = if body.is_empty() {
                String::new()
            } else {
                format!(", body: {}", quoted(body))
            };
            match pending {
                Some(review) => (
                    "submitPullRequestReview",
                    format!(
                        "pullRequestReviewId: {}, event: {}{body}",
                        quoted(review),
                        event(*verdict)
                    ),
                ),
                None => (
                    "addPullRequestReview",
                    format!(
                        "pullRequestId: {}, event: {}{body}",
                        quoted(pr),
                        event(*verdict)
                    ),
                ),
            }
        }
        PrWrite::Resolve { thread, resolved } => (
            if *resolved {
                "resolveReviewThread"
            } else {
                "unresolveReviewThread"
            },
            format!("threadId: {}", quoted(thread)),
        ),
        PrWrite::Edit {
            comment,
            kind: CommentKind::Issue,
            body,
        } => (
            "updateIssueComment",
            format!("id: {}, body: {}", quoted(comment), quoted(body)),
        ),
        PrWrite::Edit {
            comment,
            kind: CommentKind::Review,
            body,
        } => (
            "updatePullRequestReviewComment",
            format!(
                "pullRequestReviewCommentId: {}, body: {}",
                quoted(comment),
                quoted(body)
            ),
        ),
        PrWrite::Delete {
            comment,
            kind: CommentKind::Issue,
        } => ("deleteIssueComment", format!("id: {}", quoted(comment))),
        PrWrite::Delete {
            comment,
            kind: CommentKind::Review,
        } => (
            "deletePullRequestReviewComment",
            format!("id: {}", quoted(comment)),
        ),
    };
    (
        format!("mutation {{ {field}(input: {{{input}}}) {{ clientMutationId }} }}"),
        field,
    )
}

/// A mutation's answer: GitHub says no with the data still there and the field null,
/// sometimes with no message at all.
pub fn parse(v: &Value, field: &str) -> Result<(), GhState> {
    if !v["data"][field].is_null() {
        return Ok(());
    }
    let why = v["errors"][0]["message"]
        .as_str()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("GitHub did not take it");
    Err(GhState::Failed(why.to_string()))
}

/// Where a pull request is: its node id, and its repo and number for asking about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub number: u32,
}

/// Your pending review on the pull request, asked just before a review is sent: a line
/// comment a moment ago may have opened one that the detail does not know yet.
pub fn pending_query(at: &Spot) -> String {
    format!(
        "query {{ repository(owner: {}, name: {}) {{ pullRequest(number: {}) {{ reviews(states: PENDING, first: 1) {{ nodes {{ id }} }} }} }} }}",
        quoted(&at.owner),
        quoted(&at.name),
        at.number
    )
}

pub fn parse_pending(v: &Value) -> Option<String> {
    v["data"]["repository"]["pullRequest"]["reviews"]["nodes"][0]["id"]
        .as_str()
        .map(str::to_string)
}

pub fn send(gh: &dyn Gh, token: &str, write: &PrWrite, at: &Spot) -> Result<(), GhState> {
    let pending = match write {
        PrWrite::Submit { .. } => parse_pending(&gh::graphql(gh, token, &pending_query(at))?),
        _ => None,
    };
    let (query, field) = mutation(write, &at.id, pending.as_deref());
    parse(&gh::graphql(gh, token, &query)?, field)
}

/// What `write` does, for "couldn't …".
pub fn what(write: &PrWrite) -> &'static str {
    match write {
        PrWrite::Comment { .. } => "post your comment",
        PrWrite::Reply { .. } => "post your reply",
        PrWrite::LineComment { .. } => "add your comment",
        PrWrite::Submit { .. } => "send your review",
        PrWrite::Resolve { resolved: true, .. } => "resolve the thread",
        PrWrite::Resolve {
            resolved: false, ..
        } => "unresolve the thread",
        PrWrite::Edit { .. } => "edit your comment",
        PrWrite::Delete { .. } => "delete your comment",
    }
}

#[cfg(test)]
mod tests {
    use super::super::gh::fake::{FakeGh, ok};
    use super::*;

    fn json(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    fn q(write: PrWrite, pending: Option<&str>) -> String {
        mutation(&write, "PR_1", pending).0
    }

    #[test]
    fn comments_and_replies_go_at_once() {
        assert_eq!(
            q(PrWrite::Comment { body: "hi".into() }, None),
            r#"mutation { addComment(input: {subjectId: "PR_1", body: "hi"}) { clientMutationId } }"#
        );
        let reply = q(
            PrWrite::Reply {
                thread: "T1".into(),
                body: "ok".into(),
            },
            Some("PRR_1"),
        );
        assert!(
            reply.contains(r#"addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: "T1", body: "ok"})"#),
            "no review id: GitHub puts it into the pending one itself; {reply}"
        );
    }

    #[test]
    fn a_line_comment_names_its_lines_and_sides() {
        let one = q(
            PrWrite::LineComment {
                path: "src/a.rs".into(),
                side: Side::Right,
                line: 42,
                start: None,
                body: "why?".into(),
            },
            None,
        );
        assert!(one.contains(
            r#"addPullRequestReviewThread(input: {pullRequestId: "PR_1", path: "src/a.rs", line: 42, side: RIGHT, body: "why?"})"#
        ), "{one}");
        let range = q(
            PrWrite::LineComment {
                path: "src/a.rs".into(),
                side: Side::Left,
                line: 12,
                start: Some(10),
                body: "b".into(),
            },
            None,
        );
        assert!(
            range.contains("line: 12, side: LEFT, startLine: 10, startSide: LEFT"),
            "{range}"
        );
    }

    #[test]
    fn a_review_goes_through_the_pending_one_or_a_new_one() {
        let submit = q(
            PrWrite::Submit {
                verdict: Verdict::RequestChanges,
                body: "two nits".into(),
            },
            Some("PRR_1"),
        );
        assert!(submit.contains(
            r#"submitPullRequestReview(input: {pullRequestReviewId: "PRR_1", event: REQUEST_CHANGES, body: "two nits"})"#
        ), "{submit}");
        let fresh = q(
            PrWrite::Submit {
                verdict: Verdict::Approve,
                body: String::new(),
            },
            None,
        );
        assert!(
            fresh.contains(
                r#"addPullRequestReview(input: {pullRequestId: "PR_1", event: APPROVE})"#
            ),
            "an empty body is left out; {fresh}"
        );
    }

    #[test]
    fn resolving_editing_and_deleting_name_their_target() {
        let cases = [
            (
                PrWrite::Resolve {
                    thread: "T1".into(),
                    resolved: true,
                },
                r#"resolveReviewThread(input: {threadId: "T1"})"#,
            ),
            (
                PrWrite::Resolve {
                    thread: "T1".into(),
                    resolved: false,
                },
                r#"unresolveReviewThread(input: {threadId: "T1"})"#,
            ),
            (
                PrWrite::Edit {
                    comment: "IC_1".into(),
                    kind: CommentKind::Issue,
                    body: "b".into(),
                },
                r#"updateIssueComment(input: {id: "IC_1", body: "b"})"#,
            ),
            (
                PrWrite::Edit {
                    comment: "PRRC_1".into(),
                    kind: CommentKind::Review,
                    body: "b".into(),
                },
                r#"updatePullRequestReviewComment(input: {pullRequestReviewCommentId: "PRRC_1", body: "b"})"#,
            ),
            (
                PrWrite::Delete {
                    comment: "IC_1".into(),
                    kind: CommentKind::Issue,
                },
                r#"deleteIssueComment(input: {id: "IC_1"})"#,
            ),
            (
                PrWrite::Delete {
                    comment: "PRRC_1".into(),
                    kind: CommentKind::Review,
                },
                r#"deletePullRequestReviewComment(input: {id: "PRRC_1"})"#,
            ),
        ];
        for (write, want) in cases {
            let got = q(write, None);
            assert!(got.contains(want), "{got}");
        }
    }

    #[test]
    fn bodies_keep_quotes_backslashes_and_lines() {
        let got = q(
            PrWrite::Comment {
                body: "a \"word\", a \\ and\na second line".into(),
            },
            None,
        );
        assert!(
            got.contains(r#"body: "a \"word\", a \\ and\na second line""#),
            "{got}"
        );
    }

    #[test]
    fn a_refusal_says_why_or_that_github_said_nothing() {
        assert_eq!(
            parse(
                &json(r#"{"data":{"addComment":{"clientMutationId":null}}}"#),
                "addComment"
            ),
            Ok(())
        );
        let own = json(
            r#"{"data":{"addPullRequestReview":null},"errors":[{"type":"UNPROCESSABLE","message":"Review Can not approve your own pull request"}]}"#,
        );
        assert_eq!(
            parse(&own, "addPullRequestReview"),
            Err(GhState::Failed(
                "Review Can not approve your own pull request".into()
            ))
        );
        let silent = json(
            r#"{"data":{"addPullRequestReview":null},"errors":[{"type":"UNPROCESSABLE","message":""}]}"#,
        );
        assert_eq!(
            parse(&silent, "addPullRequestReview"),
            Err(GhState::Failed("GitHub did not take it".into()))
        );
    }

    fn spot() -> Spot {
        Spot {
            id: "PR_1".into(),
            owner: "acme".into(),
            name: "site".into(),
            number: 7,
        }
    }

    #[test]
    fn a_review_asks_for_the_pending_one_first() {
        let fake = FakeGh::new(|c| {
            if c.query().starts_with("query") {
                ok(
                    r#"{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[{"id":"PRR_9"}]}}}}}"#,
                )
            } else {
                ok(r#"{"data":{"submitPullRequestReview":{"clientMutationId":null}}}"#)
            }
        });
        let write = PrWrite::Submit {
            verdict: Verdict::Comment,
            body: String::new(),
        };
        assert_eq!(send(&*fake, "tok", &write, &spot()), Ok(()));
        let calls = fake.calls();
        assert!(calls[0].query().contains(
            r#"repository(owner: "acme", name: "site") { pullRequest(number: 7) { reviews(states: PENDING"#
        ));
        assert!(calls[1].query().contains(r#"pullRequestReviewId: "PRR_9""#));
        let none = FakeGh::new(|c| {
            if c.query().starts_with("query") {
                ok(r#"{"data":{"repository":{"pullRequest":{"reviews":{"nodes":[]}}}}}"#)
            } else {
                ok(r#"{"data":{"addPullRequestReview":{"clientMutationId":null}}}"#)
            }
        });
        assert_eq!(send(&*none, "tok", &write, &spot()), Ok(()));
        assert!(
            none.calls()[1]
                .query()
                .contains("addPullRequestReview(input: {pullRequestId")
        );
    }

    #[test]
    fn send_runs_the_mutation_as_the_token() {
        let fake =
            FakeGh::new(|_| ok(r#"{"data":{"deleteIssueComment":{"clientMutationId":null}}}"#));
        let write = PrWrite::Delete {
            comment: "IC_1".into(),
            kind: CommentKind::Issue,
        };
        assert_eq!(send(&*fake, "tok", &write, &spot()), Ok(()));
        assert_eq!(fake.calls().len(), 1, "only a review asks first");
        let call = &fake.calls()[0];
        assert_eq!(call.token.as_deref(), Some("tok"));
        assert!(call.query().starts_with("mutation { deleteIssueComment"));
        assert_eq!(what(&write), "delete your comment");
    }
}
