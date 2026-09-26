use crate::ids::SessionId;
use crate::model::SessionInfo;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentStatus {
    Fresh,
    Running,
    Unseen,
    Finished,
    NeedsFeedback,
    Exited { code: Option<i32> },
    Disconnected,
}

/// Everything that can move a card's status. Produced by hook events, PTY input
/// and process exit; consumed only by `AgentStatus::apply`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    PromptSubmitted,
    NeedsFeedback,
    ToolDone,
    TurnStopped,
    Cancelled,
    UserTyped,
    Seen,
    ProcessExited { code: Option<i32> },
}

impl AgentStatus {
    pub fn is_live(self) -> bool {
        !matches!(self, AgentStatus::Exited { .. } | AgentStatus::Disconnected)
    }

    pub fn apply(self, signal: Signal) -> AgentStatus {
        use AgentStatus::*;
        if let Signal::ProcessExited { code } = signal {
            return Exited { code };
        }
        if !self.is_live() {
            return self;
        }
        match (self, signal) {
            (_, Signal::PromptSubmitted) => Running,
            (_, Signal::NeedsFeedback) => NeedsFeedback,
            (NeedsFeedback | Running, Signal::ToolDone) => Running,
            (NeedsFeedback, Signal::UserTyped) => Running,
            (Fresh | Running | NeedsFeedback, Signal::TurnStopped) => Unseen,
            (Running | NeedsFeedback, Signal::Cancelled) => Finished,
            (Unseen, Signal::Seen) => Finished,
            (same, _) => same,
        }
    }

    /// Lower sorts first: what needs the user most. A crash wants a look; a clean
    /// exit ("closed") wants nothing, like a disconnected session.
    pub fn attention_rank(self) -> u8 {
        match self {
            AgentStatus::NeedsFeedback => 0,
            AgentStatus::Running => 1,
            AgentStatus::Unseen => 2,
            AgentStatus::Exited { code: Some(0) } => 6,
            AgentStatus::Exited { .. } => 3,
            AgentStatus::Fresh => 4,
            AgentStatus::Finished => 5,
            AgentStatus::Disconnected => 6,
        }
    }
}

/// Session ids sorted by attention: waiting, running, unread, …; most recent first within a rank.
pub fn attention_order(sessions: &[SessionInfo]) -> Vec<SessionId> {
    let mut v: Vec<&SessionInfo> = sessions.iter().collect();
    v.sort_by(|a, b| {
        a.status
            .attention_rank()
            .cmp(&b.status.attention_rank())
            .then(b.last_activity_ms.cmp(&a.last_activity_ms))
    });
    v.into_iter().map(|s| s.id).collect()
}

/// The session after (or before) `current` in attention order, wrapping. With no
/// current (or an unknown one) it is the head of the order.
pub fn next_in_attention(
    sessions: &[SessionInfo],
    current: Option<SessionId>,
    forward: bool,
) -> Option<SessionId> {
    let order = attention_order(sessions);
    if order.is_empty() {
        return None;
    }
    let Some(pos) = current.and_then(|c| order.iter().position(|id| *id == c)) else {
        return Some(order[0]);
    };
    let n = order.len();
    let next = if forward {
        (pos + 1) % n
    } else {
        (pos + n - 1) % n
    };
    Some(order[next])
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ProjectId, SessionId};
    use crate::model::{SessionInfo, SessionKind};
    use AgentStatus::*;

    #[test]
    fn a_full_turn_with_a_permission_prompt() {
        let s = Fresh.apply(Signal::PromptSubmitted);
        assert_eq!(s, Running);
        let s = s.apply(Signal::NeedsFeedback);
        assert_eq!(s, NeedsFeedback);
        let s = s.apply(Signal::UserTyped);
        assert_eq!(
            s, Running,
            "typing into a waiting pane is an optimistic answer"
        );
        let s = s.apply(Signal::ToolDone);
        assert_eq!(s, Running);
        let s = s.apply(Signal::TurnStopped);
        assert_eq!(s, Unseen);
        assert_eq!(s.apply(Signal::Seen), Finished);
    }

    #[test]
    fn typing_only_matters_while_waiting() {
        assert_eq!(Running.apply(Signal::UserTyped), Running);
        assert_eq!(Unseen.apply(Signal::UserTyped), Unseen);
        assert_eq!(Fresh.apply(Signal::UserTyped), Fresh);
    }

    #[test]
    fn cancel_finishes_without_an_unread_badge() {
        assert_eq!(Running.apply(Signal::Cancelled), Finished);
        assert_eq!(NeedsFeedback.apply(Signal::Cancelled), Finished);
        assert_eq!(Unseen.apply(Signal::Cancelled), Unseen);
    }

    #[test]
    fn stray_stop_and_tool_events_do_not_resurrect_a_finished_turn() {
        assert_eq!(Finished.apply(Signal::TurnStopped), Finished);
        assert_eq!(Unseen.apply(Signal::ToolDone), Unseen);
        assert_eq!(Finished.apply(Signal::Seen), Finished);
    }

    #[test]
    fn exit_wins_and_is_final() {
        let s = Running.apply(Signal::ProcessExited { code: Some(1) });
        assert_eq!(s, Exited { code: Some(1) });
        assert_eq!(s.apply(Signal::PromptSubmitted), s);
        assert_eq!(Disconnected.apply(Signal::NeedsFeedback), Disconnected);
    }

    fn info(status: AgentStatus, last: u64) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            project: ProjectId::new(),
            kind: SessionKind::Shell,
            name: "s".into(),
            status,
            agent_session_id: None,
            title: None,
            last_activity_ms: last,
        }
    }

    #[test]
    fn attention_puts_waiting_first_then_running_then_unread() {
        let v = vec![
            info(Finished, 9),
            info(Unseen, 1),
            info(NeedsFeedback, 1),
            info(Running, 5),
            info(Running, 7),
        ];
        let order = attention_order(&v);
        let statuses: Vec<_> = order
            .iter()
            .map(|id| v.iter().find(|s| s.id == *id).unwrap().status)
            .collect();
        assert_eq!(
            statuses,
            vec![NeedsFeedback, Running, Running, Unseen, Finished]
        );
        // most recent first within a rank
        assert_eq!(order[1], v[4].id);
    }

    #[test]
    fn a_crash_asks_for_attention_but_a_clean_exit_goes_last() {
        let v = vec![
            info(Exited { code: Some(0) }, 9),
            info(Disconnected, 8),
            info(Finished, 1),
            info(Fresh, 1),
            info(Exited { code: Some(2) }, 1),
            info(Exited { code: None }, 1),
            info(Unseen, 1),
        ];
        let statuses: Vec<_> = attention_order(&v)
            .iter()
            .map(|id| v.iter().find(|s| s.id == *id).unwrap().status)
            .collect();
        assert_eq!(
            statuses,
            vec![
                Unseen,
                Exited { code: Some(2) },
                Exited { code: None },
                Fresh,
                Finished,
                Exited { code: Some(0) },
                Disconnected,
            ]
        );
    }

    #[test]
    fn next_in_attention_wraps_both_ways() {
        let v = vec![info(NeedsFeedback, 1), info(Running, 1), info(Unseen, 1)];
        let first = next_in_attention(&v, None, true).unwrap();
        assert_eq!(first, v[0].id);
        assert_eq!(next_in_attention(&v, Some(v[2].id), true), Some(v[0].id));
        assert_eq!(next_in_attention(&v, Some(v[0].id), false), Some(v[2].id));
        assert_eq!(next_in_attention(&[], None, true), None);
    }
}
