use termist_core::{ClientRequest, Harness, PROTOCOL_VERSION, ServerEvent, SessionId};
use termist_daemon::hook_client::send_hook;
use termist_platform::framed::{FramedReader, write_frame};
use termist_platform::{Paths, ipc};

/// A daemon stand-in that accepts the hook, then hangs up without acknowledging it.
#[tokio::test]
async fn a_hook_that_is_never_acknowledged_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().to_path_buf());
    paths.ensure().unwrap();
    let listener = ipc::listen(&paths).unwrap();
    let server = tokio::spawn(async move {
        use interprocess::local_socket::tokio::prelude::*;
        let conn = listener.accept().await.unwrap();
        let (r, mut w) = conn.split();
        let mut reader = FramedReader::new(r);
        let _hello: Option<ClientRequest> = reader.read().await.unwrap();
        write_frame(
            &mut w,
            &ServerEvent::Hello {
                version: PROTOCOL_VERSION,
                pid: 1,
            },
        )
        .await
        .unwrap();
        let hook: Option<ClientRequest> = reader.read().await.unwrap();
        assert!(matches!(hook, Some(ClientRequest::Hook { .. })));
        // drop both halves: EOF without Ack
    });
    let err = send_hook(
        &paths,
        SessionId::new(),
        Harness::Claude,
        "Stop",
        "{}".into(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("before acknowledging"), "{err}");
    server.await.unwrap();
}
