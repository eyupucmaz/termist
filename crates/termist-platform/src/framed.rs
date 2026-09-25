use serde::Serialize;
use serde::de::DeserializeOwned;
use termist_core::codec::{FrameDecoder, encode_frame};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub struct FramedReader<R> {
    inner: R,
    decoder: FrameDecoder,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FramedReader<R> {
    pub fn new(inner: R) -> Self {
        FramedReader {
            inner,
            decoder: FrameDecoder::default(),
            buf: vec![0; 64 * 1024],
        }
    }

    /// Next message, or `None` when the peer closed the connection cleanly.
    pub async fn read<T: DeserializeOwned>(&mut self) -> anyhow::Result<Option<T>> {
        loop {
            if let Some(msg) = self.decoder.next()? {
                return Ok(Some(msg));
            }
            let n = self.inner.read(&mut self.buf).await?;
            if n == 0 {
                return Ok(None);
            }
            self.decoder.push(&self.buf[..n]);
        }
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    msg: &T,
) -> anyhow::Result<()> {
    w.write_all(&encode_frame(msg)?).await?;
    w.flush().await?;
    Ok(())
}
