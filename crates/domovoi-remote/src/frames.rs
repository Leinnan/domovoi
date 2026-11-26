//! RPC frames on an iroh stream.
//!
//! A connection made with [`crate::RPC_ALPN`] carries the RPC socket itself,
//! not HTTP: each bi-stream the client opens is one socket, and what crosses
//! it are frames, each its length (four bytes, big-endian) and then that
//! many bytes. What the bytes mean is the caller's business.

use crate::Error;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The longest frame read: one that claims more ends the stream.
pub const MAX_FRAME: usize = 256 * 1024 * 1024;

/// The reading half of an RPC stream.
pub struct FrameReader {
    recv: RecvStream,
    /// Held so the connection lives as long as either half does.
    _conn: Connection,
}

/// The writing half of an RPC stream.
pub struct FrameWriter {
    send: SendStream,
    _conn: Connection,
}

pub(crate) fn halves(conn: &Connection, send: SendStream, recv: RecvStream) -> (FrameReader, FrameWriter) {
    (FrameReader { recv, _conn: conn.clone() }, FrameWriter { send, _conn: conn.clone() })
}

impl FrameReader {
    /// The next frame, or `None` once the peer has ended the stream.
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let mut length = [0u8; 4];
        let mut read = 0;
        while read < length.len() {
            match AsyncReadExt::read(&mut self.recv, &mut length[read..]).await {
                Ok(0) if read == 0 => return Ok(None),
                Ok(0) => return Err(Error("reading a frame: the stream ended inside it".into())),
                Ok(n) => read += n,
                Err(e) => return Err(Error::msg("reading a frame", e)),
            }
        }
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_FRAME {
            return Err(Error(format!("reading a frame: {length} bytes is more than a frame holds")));
        }
        let mut frame = vec![0u8; length];
        AsyncReadExt::read_exact(&mut self.recv, &mut frame).await.map_err(|e| Error::msg("reading a frame", e))?;
        Ok(Some(frame))
    }
}

impl FrameWriter {
    pub async fn send(&mut self, frame: &[u8]) -> Result<(), Error> {
        let length = u32::try_from(frame.len()).ok().filter(|_| frame.len() <= MAX_FRAME).ok_or_else(|| Error(format!("writing a frame: {} bytes is more than a frame holds", frame.len())))?;
        AsyncWriteExt::write_all(&mut self.send, &length.to_be_bytes()).await.map_err(|e| Error::msg("writing a frame", e))?;
        AsyncWriteExt::write_all(&mut self.send, frame).await.map_err(|e| Error::msg("writing a frame", e))
    }

    /// Ends the stream once the peer has everything that was sent (or has
    /// gone): it then reads the stream's end and not a reset.
    pub async fn finish(mut self) {
        if self.send.finish().is_ok() {
            let _ = self.send.stopped().await;
        }
    }
}
