use crate::framed::{FramedReader, write_frame};
use crate::ipc::{self, RecvHalf, SendHalf, Stream};
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
        Self::handshake(ipc::connect(paths).await?).await
    }

    /// Performs the Hello handshake on a connected stream.
    pub async fn handshake(stream: Stream) -> anyhow::Result<Client> {
        let mut client = Self::without_handshake(stream);
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

    /// A connection that skipped the handshake. The only request a daemon takes without
    /// a Hello is `Shutdown` (daemons older than that rule just close the connection).
    pub fn without_handshake(stream: Stream) -> Client {
        let (r, w) = stream.split();
        Client {
            reader: FramedReader::new(r),
            writer: w,
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
