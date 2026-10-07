use serde::Serialize;
use serde::de::DeserializeOwned;

/// Frames bigger than this are refused on both sides.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("frame of {0} bytes exceeds the {MAX_FRAME} byte limit")]
    TooLarge(usize),
    #[error("encode: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("decode: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
}

/// `u32` big-endian length, then a MessagePack body with named fields.
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, CodecError> {
    let body = rmp_serde::to_vec_named(msg)?;
    if body.len() > MAX_FRAME {
        return Err(CodecError::TooLarge(body.len()));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next<T: DeserializeOwned>(&mut self) -> Result<Option<T>, CodecError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_FRAME {
            return Err(CodecError::TooLarge(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let msg = rmp_serde::from_slice(&self.buf[4..4 + len])?;
        self.buf.drain(..4 + len);
        Ok(Some(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{ClientRequest, ServerEvent};

    /// Shutdown and Ack travel by name, not by position, so daemons and clients of
    /// different protocol versions still understand each other's.
    #[test]
    fn shutdown_and_ack_are_encoded_by_name() {
        let name = |s: &str| rmp_serde::to_vec(s).unwrap();
        assert_eq!(
            encode_frame(&ClientRequest::Shutdown).unwrap()[4..],
            name("Shutdown")[..]
        );
        assert_eq!(
            encode_frame(&ServerEvent::Ack).unwrap()[4..],
            name("Ack")[..]
        );
    }
    use crate::{Harness, SessionId};

    #[test]
    fn frames_round_trip_even_when_split_byte_by_byte() {
        let req = ClientRequest::Hook {
            session: SessionId::new(),
            harness: Harness::Claude,
            event: "Stop".into(),
            payload_json: "{}".into(),
        };
        let bytes = encode_frame(&req).unwrap();
        let mut dec = FrameDecoder::default();
        for b in &bytes[..bytes.len() - 1] {
            dec.push(std::slice::from_ref(b));
            assert!(dec.next::<ClientRequest>().unwrap().is_none());
        }
        dec.push(&bytes[bytes.len() - 1..]);
        assert_eq!(dec.next::<ClientRequest>().unwrap(), Some(req));
    }

    #[test]
    fn two_frames_in_one_read() {
        let mut bytes = encode_frame(&ServerEvent::Ack).unwrap();
        bytes.extend(
            encode_frame(&ServerEvent::Error {
                message: "x".into(),
            })
            .unwrap(),
        );
        let mut dec = FrameDecoder::default();
        dec.push(&bytes);
        assert_eq!(dec.next::<ServerEvent>().unwrap(), Some(ServerEvent::Ack));
        assert_eq!(
            dec.next::<ServerEvent>().unwrap(),
            Some(ServerEvent::Error {
                message: "x".into()
            })
        );
        assert_eq!(dec.next::<ServerEvent>().unwrap(), None);
    }

    #[test]
    fn oversized_length_header_is_rejected_before_buffering() {
        let mut dec = FrameDecoder::default();
        dec.push(&(u32::MAX).to_be_bytes());
        assert!(matches!(
            dec.next::<ServerEvent>(),
            Err(CodecError::TooLarge(_))
        ));
    }

    #[test]
    fn input_bytes_travel_as_binary() {
        let req = ClientRequest::Input {
            session: SessionId::new(),
            data: vec![0xff; 100],
        };
        let bytes = encode_frame(&req).unwrap();
        assert!(
            bytes.len() < 220,
            "Vec<u8> must be encoded as a msgpack bin, not an int array"
        );
    }

    #[test]
    fn the_new_messages_round_trip() {
        use crate::model::{Harness, HarnessInfo};
        let ev = ServerEvent::Harnesses(vec![HarnessInfo {
            harness: Harness::OpenCode,
            available: false,
        }]);
        let mut dec = FrameDecoder::default();
        dec.push(&encode_frame(&ev).unwrap());
        assert_eq!(dec.next::<ServerEvent>().unwrap(), Some(ev));
        let req = ClientRequest::Resume {
            session: SessionId::new(),
            cols: 80,
            rows: 24,
        };
        dec.push(&encode_frame(&req).unwrap());
        assert_eq!(dec.next::<ClientRequest>().unwrap(), Some(req));
    }

    #[test]
    fn the_interaction_messages_round_trip() {
        use crate::model::{Harness, LaunchOptions};
        let mut dec = FrameDecoder::default();
        let requests = [
            ClientRequest::CreateSession {
                project: crate::ProjectId::new(),
                kind: crate::SessionKind::Agent {
                    harness: Harness::Codex,
                },
                cwd: None,
                prompt: Some("fix it".into()),
                model: Some("gpt 5 \"x\"".into()),
                effort: Some("high".into()),
                cols: 80,
                rows: 24,
            },
            ClientRequest::SetLastLaunch(LaunchOptions {
                harness: Harness::Claude,
                model: None,
                effort: Some("max".into()),
            }),
            ClientRequest::RenameSession {
                session: SessionId::new(),
                name: "login bug".into(),
            },
            ClientRequest::RescanHarnesses,
            ClientRequest::SetColors(crate::TermColors {
                fg: (1, 2, 3),
                bg: (4, 5, 6),
                ansi: Some([(7, 8, 9); 16]),
            }),
        ];
        for req in requests {
            dec.push(&encode_frame(&req).unwrap());
            assert_eq!(dec.next::<ClientRequest>().unwrap(), Some(req));
        }
        let events = [
            ServerEvent::PromptHistory(vec!["b".into(), "a".into()]),
            ServerEvent::Models {
                harness: Harness::OpenCode,
                recent: vec!["anthropic/claude-sonnet-4-5".into()],
                catalog: vec![crate::ModelInfo {
                    id: "gpt-6-astra".into(),
                    label: "GPT-6-Astra".into(),
                    efforts: vec!["low".into(), "ultra".into()],
                }],
            },
        ];
        for ev in events {
            dec.push(&encode_frame(&ev).unwrap());
            assert_eq!(dec.next::<ServerEvent>().unwrap(), Some(ev));
        }
    }

    #[test]
    fn the_github_messages_round_trip() {
        use crate::github::*;
        let pr = PrRef {
            repo: RepoId(7),
            number: 212,
        };
        let summary = PrSummary {
            number: 212,
            title: "Add a dealer filter".into(),
            url: "https://github.com/acme/site/pull/212".into(),
            author: "bob".into(),
            draft: false,
            state: PrState::Open,
            created_at: "2026-10-01T10:00:00Z".into(),
            updated_at: "2026-10-02T10:00:00Z".into(),
            head: "feat/dealer".into(),
            head_repo: "acme/site".into(),
            base: "main".into(),
            additions: 184,
            deletions: 32,
            changed_files: 9,
            mergeable: Mergeable::Unknown,
            decision: Some(ReviewDecision::ReviewRequired),
            requested: vec!["alice".into(), "@acme/web".into()],
            requested_you: true,
            verdicts: vec![("carol".into(), ReviewState::Approved)],
            checks: Checks::Passing,
            unseen: true,
        };
        let mut dec = FrameDecoder::default();
        let requests = [
            ClientRequest::SetGitHub { enabled: true },
            ClientRequest::SetPrFocus {
                project: Some(crate::ProjectId::new()),
                pr: Some(pr),
                diff: true,
            },
            ClientRequest::SetFileViewed {
                pr,
                path: "src/a \"b\".rs".into(),
                viewed: true,
            },
            ClientRequest::WritePr {
                pr,
                ticket: 7,
                write: PrWrite::LineComment {
                    path: "src/a.rs".into(),
                    side: Side::Right,
                    line: 42,
                    start: Some(40),
                    body: "a \"quoted\" word, a \\ backslash\nand a second line".into(),
                },
            },
            ClientRequest::WritePr {
                pr,
                ticket: 8,
                write: PrWrite::Submit {
                    verdict: Verdict::RequestChanges,
                    body: "two nits".into(),
                },
            },
            ClientRequest::WritePr {
                pr,
                ticket: 9,
                write: PrWrite::Delete {
                    comment: "IC_1".into(),
                    kind: CommentKind::Issue,
                },
            },
            ClientRequest::SetRepoAccount {
                repo: RepoId(7),
                account: None,
            },
            ClientRequest::MarkPrSeen {
                pr,
                updated_at: "2026-10-02T10:00:00Z".into(),
            },
        ];
        for req in requests {
            dec.push(&encode_frame(&req).unwrap());
            assert_eq!(dec.next::<ClientRequest>().unwrap(), Some(req));
        }
        let events = [
            ServerEvent::Prs {
                project: crate::ProjectId::new(),
                state: GhState::RateLimited {
                    reset_at: "2026-10-02T11:00:00Z".into(),
                },
                discovered: 2,
                repos: vec![RepoPrs {
                    repo: RepoId(7),
                    name: "site".into(),
                    slug: "acme/site".into(),
                    state: GhState::Ok,
                    viewer: Some("alice".into()),
                    prs: vec![summary.clone()],
                    total: 1,
                    fetched_at: Some("2026-10-02T10:00:05Z".into()),
                    failed_at: None,
                }],
            },
            ServerEvent::PrDetail {
                pr,
                state: GhState::Ok,
                detail: Some(Box::new(PrDetail {
                    summary,
                    id: "PR_kw1".into(),
                    head_oid: "b4d4d37".into(),
                    mine: true,
                    pending_review: Some("PRR_1".into()),
                    body: "Closes #198.".into(),
                    comments: vec![],
                    reviews: vec![],
                    threads: vec![Thread {
                        id: "T1".into(),
                        path: "src/a.rs".into(),
                        line: Some(42),
                        start_line: Some(40),
                        side: Side::Left,
                        resolved: false,
                        outdated: false,
                        hunk: "@@ -1 +1 @@\n-a\n+b".into(),
                        comments: vec![Comment {
                            id: "PRRC_1".into(),
                            author: "carol".into(),
                            body: "why?".into(),
                            created_at: "2026-10-02T09:00:00Z".into(),
                            mine: false,
                            can_edit: false,
                            can_delete: false,
                            pending: true,
                        }],
                        more: 0,
                        can_reply: true,
                        can_resolve: false,
                    }],
                    checks: vec![],
                    files: vec![FileChange {
                        path: "src/a.rs".into(),
                        additions: 1,
                        deletions: 1,
                        change: 'M',
                        viewed: Viewed::Dismissed,
                    }],
                    more: More::default(),
                })),
            },
            ServerEvent::PrDiff {
                pr,
                state: GhState::Ok,
                diff: Some(Box::new(PrDiff {
                    head_oid: "b4d4d37".into(),
                    files: vec![
                        DiffFile {
                            path: "src/a.rs".into(),
                            previous: Some("src/old.rs".into()),
                            change: 'R',
                            additions: 1,
                            deletions: 1,
                            viewed: Viewed::Viewed,
                            patch: Patch::Text("@@ -1 +1 @@\n-a\n+b".into()),
                            url: "https://github.com/acme/site/pull/212/files#diff-ab".into(),
                        },
                        DiffFile {
                            path: "logo.png".into(),
                            previous: None,
                            change: 'A',
                            additions: 0,
                            deletions: 0,
                            viewed: Viewed::Unviewed,
                            patch: Patch::Binary,
                            url: String::new(),
                        },
                    ],
                    more: 3,
                })),
            },
            ServerEvent::PrWriteFailed {
                pr,
                ticket: None,
                message: "couldn't mark src/a.rs viewed: Resource not accessible".into(),
            },
            ServerEvent::PrWriteFailed {
                pr,
                ticket: Some(7),
                message:
                    "couldn't post your comment · Review Can not approve your own pull request"
                        .into(),
            },
            ServerEvent::PrWritten { pr, ticket: 7 },
            ServerEvent::ReviewRequested {
                project: crate::ProjectId::new(),
                pr,
                repo: "site".into(),
                title: "Add a dealer filter".into(),
            },
        ];
        for ev in events {
            dec.push(&encode_frame(&ev).unwrap());
            assert_eq!(dec.next::<ServerEvent>().unwrap(), Some(ev));
        }
    }

    #[test]
    fn the_worktree_messages_and_a_session_s_place_round_trip() {
        use crate::github::{PrRef, RepoId};
        use crate::model::{Place, SessionInfo, SessionKind};
        let pr = PrRef {
            repo: RepoId(7),
            number: 212,
        };
        let mut dec = FrameDecoder::default();
        let requests = [
            ClientRequest::OpenWorktree { pr },
            ClientRequest::CreateSession {
                project: crate::ProjectId::new(),
                kind: SessionKind::Shell,
                cwd: Some("/w/site-worktrees/fix/login \"x\"".into()),
                prompt: None,
                model: None,
                effort: None,
                cols: 80,
                rows: 24,
            },
        ];
        for req in requests {
            dec.push(&encode_frame(&req).unwrap());
            assert_eq!(dec.next::<ClientRequest>().unwrap(), Some(req));
        }
        let session = SessionInfo {
            id: crate::SessionId::new(),
            project: crate::ProjectId::new(),
            kind: SessionKind::Shell,
            name: "shell-1".into(),
            status: crate::AgentStatus::Fresh,
            agent_session_id: None,
            title: None,
            last_activity_ms: 0,
            model: None,
            effort: None,
            user_named: false,
            archived: false,
            cwd: "/w/site-worktrees/fix/login/src".into(),
            place: Some(Box::new(Place {
                root: "/w/site-worktrees/fix/login".into(),
                branch: Some("fix/login".into()),
                commit: None,
                repo: Some(RepoId(7)),
                pr: Some(pr),
                gone: false,
            })),
        };
        let events = [
            ServerEvent::SessionUpdated(session),
            ServerEvent::WorktreeReady {
                pr,
                path: "/w/site-worktrees/fix/login".into(),
                created: true,
            },
            ServerEvent::WorktreeFailed {
                pr,
                message: "couldn't open a worktree · fatal: 'x' is not a valid branch name".into(),
            },
        ];
        for ev in events {
            dec.push(&encode_frame(&ev).unwrap());
            assert_eq!(dec.next::<ServerEvent>().unwrap(), Some(ev));
        }
    }
}
