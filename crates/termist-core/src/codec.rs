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
}
