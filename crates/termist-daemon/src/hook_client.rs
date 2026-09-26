use std::time::Duration;
use termist_core::{ClientRequest, Harness, ServerEvent, SessionId};
use termist_platform::{Client, Paths};

pub async fn send_hook(
    paths: &Paths,
    session: SessionId,
    harness: Harness,
    event: &str,
    payload_json: String,
) -> anyhow::Result<()> {
    let mut client = Client::connect(paths).await?;
    client
        .send(&ClientRequest::Hook {
            session,
            harness,
            event: event.to_string(),
            payload_json,
        })
        .await?;
    while let Some(ev) = client.recv().await? {
        if ev == ServerEvent::Ack {
            return Ok(());
        }
    }
    anyhow::bail!("the daemon closed the connection before acknowledging the hook")
}

/// What `termist hook` does. Never errors and never outlives `limit`: a hook must not
/// slow down or break the agent that called it.
pub async fn run_hook(
    paths: &Paths,
    harness: &str,
    event: &str,
    stdin: String,
    session_env: Option<String>,
    limit: Duration,
) -> bool {
    let (Some(harness), Some(session)) = (
        Harness::from_id(harness),
        session_env.and_then(|s| s.parse::<SessionId>().ok()),
    ) else {
        return false;
    };
    matches!(
        tokio::time::timeout(limit, send_hook(paths, session, harness, event, stdin)).await,
        Ok(Ok(()))
    )
}
