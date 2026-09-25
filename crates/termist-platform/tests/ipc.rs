use termist_core::{ClientRequest, PROTOCOL_VERSION, ServerEvent};
use termist_platform::framed::{FramedReader, write_frame};
use termist_platform::{Client, Paths, ipc};

#[tokio::test]
async fn client_handshake_and_one_request_over_a_real_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().to_path_buf());
    paths.ensure().unwrap();
    let listener = ipc::listen(&paths).unwrap();

    let server = tokio::spawn(async move {
        use interprocess::local_socket::tokio::prelude::*;
        let conn = listener.accept().await.unwrap();
        let (r, mut w) = conn.split();
        let mut reader = FramedReader::new(r);
        assert_eq!(
            reader.read::<ClientRequest>().await.unwrap(),
            Some(ClientRequest::Hello {
                version: PROTOCOL_VERSION
            })
        );
        write_frame(
            &mut w,
            &ServerEvent::Hello {
                version: PROTOCOL_VERSION,
                pid: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            reader.read::<ClientRequest>().await.unwrap(),
            Some(ClientRequest::ListState)
        );
        write_frame(&mut w, &ServerEvent::Ack).await.unwrap();
    });

    let mut client = Client::connect(&paths).await.unwrap();
    client.send(&ClientRequest::ListState).await.unwrap();
    assert_eq!(client.recv().await.unwrap(), Some(ServerEvent::Ack));
    server.await.unwrap();
}

#[tokio::test]
async fn connecting_with_nobody_listening_fails_fast() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::under(tmp.path().to_path_buf());
    paths.ensure().unwrap();
    let r = tokio::time::timeout(std::time::Duration::from_secs(2), Client::connect(&paths)).await;
    assert!(matches!(r, Ok(Err(_))), "must error, not hang");
}
