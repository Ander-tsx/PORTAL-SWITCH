// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Reading and writing protocol frames on a byte stream.

use portal_proto::{Channel, Decoded, Message, decode, encode};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// Why a link operation failed.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Proto(#[from] portal_proto::Error),
    #[error("the peer closed the connection")]
    Closed,
    #[error("timed out")]
    Timeout,
}

/// Reads frames from one channel of a link.
#[derive(Debug)]
pub struct FrameReader<R> {
    inner: R,
    channel: Channel,
    buf: Vec<u8>,
    start: usize,
    /// Heap scratch space, so `read`'s future stays small.
    chunk: Box<[u8]>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(inner: R, channel: Channel) -> Self {
        Self {
            inner,
            channel,
            buf: Vec::with_capacity(4096),
            start: 0,
            chunk: vec![0; 16 * 1024].into(),
        }
    }

    /// Wait for the next complete frame.
    ///
    /// # Errors
    /// [`LinkError::Closed`] on a clean end of stream between frames; I/O and protocol errors
    /// otherwise (a stream ending mid-frame is an I/O `UnexpectedEof`).
    pub async fn read(&mut self) -> Result<Decoded, LinkError> {
        loop {
            if let Some((decoded, used)) = decode(self.channel, &self.buf[self.start..])? {
                self.start += used;
                if self.start == self.buf.len() {
                    self.buf.clear();
                    self.start = 0;
                } else if self.start > 256 * 1024 {
                    self.buf.drain(..self.start);
                    self.start = 0;
                }
                return Ok(decoded);
            }
            let n = self.inner.read(&mut self.chunk).await?;
            if n == 0 {
                return Err(if self.start == self.buf.len() {
                    LinkError::Closed
                } else {
                    std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into()
                });
            }
            self.buf.extend_from_slice(&self.chunk[..n]);
        }
    }

    /// Next *known* message; frames of unknown non-critical types are skipped.
    ///
    /// # Errors
    /// See [`FrameReader::read`].
    pub async fn read_message(&mut self) -> Result<Message, LinkError> {
        loop {
            if let Decoded::Message(m) = self.read().await? {
                return Ok(m);
            }
        }
    }
}

/// Writes frames to one channel of a link.
#[derive(Debug)]
pub struct FrameWriter<W> {
    inner: W,
    scratch: Vec<u8>,
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            scratch: Vec::with_capacity(512),
        }
    }

    /// Encode and write one message, flushing so latency-critical frames leave immediately.
    ///
    /// # Errors
    /// Encoding limits exceeded, or the stream failed.
    pub async fn write(&mut self, message: &Message) -> Result<(), LinkError> {
        self.scratch.clear();
        encode(message, &mut self.scratch)?;
        self.inner.write_all(&self.scratch).await?;
        self.inner.flush().await?;
        Ok(())
    }

    /// Shut the write side down.
    ///
    /// # Errors
    /// The stream failed.
    pub async fn shutdown(&mut self) -> Result<(), LinkError> {
        self.inner.shutdown().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use portal_proto::{Ping, TransferChunk};
    use tokio::io::duplex;

    use super::*;

    #[tokio::test]
    async fn messages_round_trip_over_a_stream() {
        let (a, b) = duplex(1024);
        let mut writer = FrameWriter::new(a);
        let mut reader = FrameReader::new(b, Channel::Control);
        for nonce in 0..50 {
            writer
                .write(&Message::Ping(Ping {
                    nonce,
                    sender_time_us: 9,
                }))
                .await
                .unwrap();
        }
        for nonce in 0..50 {
            assert_eq!(
                reader.read_message().await.unwrap(),
                Message::Ping(Ping {
                    nonce,
                    sender_time_us: 9
                })
            );
        }
    }

    #[tokio::test]
    async fn large_bulk_frames_survive_small_pipes() {
        let (a, b) = duplex(1000);
        let chunk = Message::TransferChunk(TransferChunk {
            transfer_id: 1,
            item_index: 0,
            offset: 0,
            data: vec![7; 200_000],
        });
        let expected = chunk.clone();
        let sender = tokio::spawn(async move {
            FrameWriter::new(a).write(&chunk).await.unwrap();
        });
        let mut reader = FrameReader::new(b, Channel::Bulk);
        assert_eq!(reader.read_message().await.unwrap(), expected);
        sender.await.unwrap();
    }

    #[tokio::test]
    async fn clean_close_between_frames_is_closed_but_mid_frame_is_an_error() {
        let (a, b) = duplex(64);
        drop(a);
        let mut reader = FrameReader::new(b, Channel::Control);
        assert!(matches!(reader.read().await, Err(LinkError::Closed)));

        let (mut a, b) = duplex(64);
        a.write_all(&[9, 0, 0, 0, 4]).await.unwrap(); // header promises more than will come
        drop(a);
        let mut reader = FrameReader::new(b, Channel::Control);
        assert!(matches!(reader.read().await, Err(LinkError::Io(_))));
    }

    #[tokio::test]
    async fn oversized_length_is_rejected_before_buffering() {
        let (mut a, b) = duplex(64);
        a.write_all(&[0xff, 0xff, 0xff, 0x7f]).await.unwrap();
        let mut reader = FrameReader::new(b, Channel::Control);
        assert!(matches!(
            reader.read().await,
            Err(LinkError::Proto(portal_proto::Error::FrameTooLarge { .. }))
        ));
    }

    #[tokio::test]
    async fn unknown_noncritical_frames_are_skipped() {
        let (mut a, b) = duplex(256);
        a.write_all(&[5, 0, 0, 0, 0x01, 0x7f, 1, 2, 3])
            .await
            .unwrap();
        FrameWriter::new(a)
            .write(&Message::PairStart)
            .await
            .unwrap();
        let mut reader = FrameReader::new(b, Channel::Control);
        assert_eq!(reader.read_message().await.unwrap(), Message::PairStart);
    }
}
