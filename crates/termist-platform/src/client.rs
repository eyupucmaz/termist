use crate::framed::{FramedReader, write_frame};
use crate::ipc::{self, RecvHalf, SendHalf};
use crate::paths::Paths;
use anyhow::bail;
use interprocess::local_socket::tokio::prelude::*;
use termist_core::{ClientRequest, PROTOCOL_VERSION, ServerEvent};

pub struct Client {
    reader: FramedReader<RecvHalf>,
    writer: SendHalf,
}

impl Client {
    /// Connects and performs the Hello handshake.
    pub async fn connect(paths: &Paths) -> anyhow::Result<Client> {
        let stream = ipc::connect(paths).await?;
        let (r, w) = stream.split();
        let mut client = Client {
            reader: FramedReader::new(r),
            writer: w,
        };
        client
            .send(&ClientRequest::Hello {
                version: PROTOCOL_VERSION,
            })
            .await?;
        match client.recv().await? {
            Some(ServerEvent::Hello { version, .. }) if version == PROTOCOL_VERSION => Ok(client),
            Some(ServerEvent::Error { message }) => {
                bail!("daemon refused the connection: {message}")
            }
            other => bail!("unexpected handshake reply: {other:?}"),
        }
    }

    pub async fn send(&mut self, req: &ClientRequest) -> anyhow::Result<()> {
        write_frame(&mut self.writer, req).await
    }

    pub async fn recv(&mut self) -> anyhow::Result<Option<ServerEvent>> {
        self.reader.read().await
    }

    pub fn into_split(self) -> (FramedReader<RecvHalf>, SendHalf) {
        (self.reader, self.writer)
    }
}
