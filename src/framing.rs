//! Newline-delimited JSON framing shared by every control-channel peer.
//!
//! One frame is one JSON document followed by `\n` (a trailing `\r` is
//! tolerated). Reads are bounded: a peer that never sends a newline cannot
//! grow memory past the codec limit. After a size or truncation error the
//! stream is no longer frame-aligned and must be dropped; a JSON error leaves
//! the stream aligned, so callers may skip that frame and keep reading.

use crate::error::{LabyrinthError, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Authenticated control traffic carries file uploads and in-memory payloads.
pub const MAX_CONTROL_FRAME: usize = 64 * 1024 * 1024;
/// Pre-authentication frames (registration, dweller hello) stay small.
pub const MAX_HANDSHAKE_FRAME: usize = 256 * 1024;
/// Per-stream setup frames on native QUIC Portal streams.
pub const MAX_SETUP_FRAME: usize = 16 * 1024;
/// Upper bound for a peer to finish a handshake step.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameCodec {
    max_frame_len: usize,
}

impl FrameCodec {
    pub const CONTROL: Self = Self::new(MAX_CONTROL_FRAME);
    pub const HANDSHAKE: Self = Self::new(MAX_HANDSHAKE_FRAME);
    pub const SETUP: Self = Self::new(MAX_SETUP_FRAME);

    /// `max_frame_len` bounds the JSON payload, excluding the delimiter.
    pub const fn new(max_frame_len: usize) -> Self {
        Self { max_frame_len }
    }

    pub const fn max_frame_len(self) -> usize {
        self.max_frame_len
    }

    pub fn encode<T: Serialize + ?Sized>(self, value: &T) -> Result<Vec<u8>> {
        let mut frame = serde_json::to_vec(value)?;
        if frame.len() > self.max_frame_len {
            // Refuse to send what the peer is required to reject.
            return Err(LabyrinthError::FrameTooLarge {
                limit: self.max_frame_len,
            });
        }
        frame.push(b'\n');
        Ok(frame)
    }

    pub fn decode<T: DeserializeOwned>(self, line: &[u8]) -> Result<T> {
        let line = strip_delimiter(line);
        if line.len() > self.max_frame_len {
            return Err(LabyrinthError::FrameTooLarge {
                limit: self.max_frame_len,
            });
        }
        Ok(serde_json::from_slice(line)?)
    }

    /// Encode and write one frame with a single `write_all`, so concurrent
    /// writers serialized by a lock never interleave partial frames.
    pub async fn write<W, T>(self, writer: &mut W, value: &T) -> Result<()>
    where
        W: AsyncWrite + Unpin + ?Sized,
        T: Serialize + ?Sized,
    {
        let frame = self.encode(value)?;
        writer.write_all(&frame).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Read one frame. `Ok(None)` is a clean EOF on a frame boundary.
    /// Blank lines are treated as keepalives and skipped.
    ///
    /// Not cancel safe: a partially read frame is lost if the future is
    /// dropped. Use [`FrameReader`] inside `tokio::select!`.
    pub async fn read<R, T>(self, reader: &mut R) -> Result<Option<T>>
    where
        R: AsyncBufRead + Unpin + ?Sized,
        T: DeserializeOwned,
    {
        FrameReader::new(reader, self).next().await
    }

    /// Like [`read`](Self::read) but EOF is an error and the whole read is
    /// bounded by `deadline`. Intended for request/response handshake steps.
    pub async fn read_required<R, T>(self, reader: &mut R, deadline: Duration) -> Result<T>
    where
        R: AsyncBufRead + Unpin + ?Sized,
        T: DeserializeOwned,
    {
        match tokio::time::timeout(deadline, self.read(reader)).await {
            Err(_) => Err(LabyrinthError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for frame",
            ))),
            Ok(Ok(Some(value))) => Ok(value),
            Ok(Ok(None)) => Err(LabyrinthError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "peer closed connection before sending a frame",
            ))),
            Ok(Err(error)) => Err(error),
        }
    }

    /// Bytes `read_until` may consume for one frame: payload + optional
    /// `\r` + `\n`, plus one byte to prove the limit was exceeded.
    fn read_budget(self) -> usize {
        self.max_frame_len.saturating_add(3)
    }
}

/// Cancel-safe frame reader. Partial frames are kept across dropped `next()`
/// futures, so it is safe as a `tokio::select!` branch.
pub struct FrameReader<R> {
    inner: R,
    codec: FrameCodec,
    line: Vec<u8>,
}

impl<R> FrameReader<R>
where
    R: AsyncBufRead + Unpin,
{
    pub fn new(inner: R, codec: FrameCodec) -> Self {
        Self {
            inner,
            codec,
            line: Vec::new(),
        }
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Return the underlying reader. Any partially read frame is discarded.
    pub fn into_inner(self) -> R {
        self.inner
    }

    pub async fn next<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        loop {
            while self.line.last() != Some(&b'\n') {
                let remaining = self.codec.read_budget().saturating_sub(self.line.len());
                if remaining == 0 {
                    self.line.clear();
                    return Err(LabyrinthError::FrameTooLarge {
                        limit: self.codec.max_frame_len,
                    });
                }
                let read = (&mut self.inner)
                    .take(remaining as u64)
                    .read_until(b'\n', &mut self.line)
                    .await?;
                if read == 0 {
                    if self.line.is_empty() {
                        return Ok(None);
                    }
                    self.line.clear();
                    return Err(LabyrinthError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed mid-frame",
                    )));
                }
            }
            let line = std::mem::take(&mut self.line);
            if strip_delimiter(&line).is_empty() {
                continue;
            }
            return self.codec.decode(&line).map(Some);
        }
    }
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self::CONTROL
    }
}

fn strip_delimiter(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Write one control frame using the default codec.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
    T: Serialize + ?Sized,
{
    FrameCodec::CONTROL.write(writer, value).await
}

/// Read one control frame using the default codec.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncBufRead + Unpin + ?Sized,
    T: DeserializeOwned,
{
    FrameCodec::CONTROL.read(reader).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Message;
    use tokio::io::{duplex, AsyncWriteExt, BufReader};

    fn reader(bytes: &[u8]) -> BufReader<std::io::Cursor<Vec<u8>>> {
        BufReader::new(std::io::Cursor::new(bytes.to_vec()))
    }

    #[test]
    fn encode_appends_single_newline_and_has_no_embedded_newlines() {
        let frame = FrameCodec::CONTROL
            .encode(&Message::CommandRequest {
                command: "echo a\nb".into(),
            })
            .unwrap();
        assert_eq!(frame.last(), Some(&b'\n'));
        assert_eq!(frame.iter().filter(|b| **b == b'\n').count(), 1);
    }

    #[test]
    fn encode_rejects_frames_over_limit() {
        let codec = FrameCodec::new(8);
        let error = codec
            .encode(&Message::CommandRequest {
                command: "x".repeat(32),
            })
            .unwrap_err();
        assert!(matches!(error, LabyrinthError::FrameTooLarge { limit: 8 }));
    }

    #[test]
    fn decode_tolerates_crlf_and_missing_delimiter() {
        let codec = FrameCodec::CONTROL;
        assert!(matches!(
            codec.decode::<Message>(b"\"Ping\"\r\n").unwrap(),
            Message::Ping
        ));
        assert!(matches!(
            codec.decode::<Message>(b"\"Pong\"").unwrap(),
            Message::Pong
        ));
    }

    #[tokio::test]
    async fn round_trips_over_duplex_stream() {
        let (mut client, server) = duplex(1024);
        let mut server = BufReader::new(server);
        write_frame(&mut client, &Message::Ping).await.unwrap();
        write_frame(
            &mut client,
            &Message::CommandRequest {
                command: "whoami".into(),
            },
        )
        .await
        .unwrap();
        drop(client);

        assert!(matches!(
            read_frame::<_, Message>(&mut server).await.unwrap(),
            Some(Message::Ping)
        ));
        match read_frame::<_, Message>(&mut server).await.unwrap() {
            Some(Message::CommandRequest { command }) => assert_eq!(command, "whoami"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(read_frame::<_, Message>(&mut server)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn reads_frames_split_across_many_small_writes() {
        let (mut client, server) = duplex(4);
        let writer = tokio::spawn(async move {
            for chunk in b"\"Ping\"\n\"Pong\"\n".chunks(3) {
                client.write_all(chunk).await.unwrap();
            }
        });
        let mut server = BufReader::new(server);
        assert!(matches!(
            read_frame::<_, Message>(&mut server).await.unwrap(),
            Some(Message::Ping)
        ));
        assert!(matches!(
            read_frame::<_, Message>(&mut server).await.unwrap(),
            Some(Message::Pong)
        ));
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn empty_stream_is_clean_eof_not_panic() {
        // Regression: `buf[..buf.len() - 1]` underflowed on immediate close.
        let mut input = reader(b"");
        assert!(read_frame::<_, Message>(&mut input)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn blank_lines_are_skipped_as_keepalives() {
        let mut input = reader(b"\n\r\n\n\"Ping\"\n");
        assert!(matches!(
            read_frame::<_, Message>(&mut input).await.unwrap(),
            Some(Message::Ping)
        ));
    }

    #[tokio::test]
    async fn truncated_frame_is_unexpected_eof() {
        let mut input = reader(b"\"Pi");
        match read_frame::<_, Message>(&mut input).await {
            Err(LabyrinthError::Io(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof)
            }
            other => panic!("expected UnexpectedEof, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_json_is_recoverable_and_stream_stays_aligned() {
        let mut input = reader(b"{not json}\n\"Pong\"\n");
        assert!(matches!(
            read_frame::<_, Message>(&mut input).await,
            Err(LabyrinthError::Json(_))
        ));
        assert!(matches!(
            read_frame::<_, Message>(&mut input).await.unwrap(),
            Some(Message::Pong)
        ));
    }

    #[tokio::test]
    async fn oversized_frame_without_newline_is_bounded() {
        let codec = FrameCodec::new(16);
        // Far more than the limit and never a newline: must not buffer it all.
        let mut input = reader(&vec![b'a'; 1024 * 1024]);
        match codec.read::<_, Message>(&mut input).await {
            Err(LabyrinthError::FrameTooLarge { limit }) => assert_eq!(limit, 16),
            other => panic!("expected FrameTooLarge, got {other:?}"),
        }
        assert!(
            input.get_ref().position() < 64 * 1024,
            "codec consumed {} bytes past a 16 byte limit",
            input.get_ref().position()
        );
    }

    #[tokio::test]
    async fn oversized_frame_with_newline_is_rejected() {
        let codec = FrameCodec::new(4);
        let mut input = reader(b"\"Ping\"\n");
        assert!(matches!(
            codec.read::<_, Message>(&mut input).await,
            Err(LabyrinthError::FrameTooLarge { limit: 4 })
        ));
    }

    #[tokio::test]
    async fn frame_exactly_at_limit_is_accepted_with_crlf() {
        let payload = br#""Ping""#;
        let codec = FrameCodec::new(payload.len());
        let mut input = reader(b"\"Ping\"\r\n");
        assert!(matches!(
            codec.read::<_, Message>(&mut input).await.unwrap(),
            Some(Message::Ping)
        ));
    }

    #[tokio::test]
    async fn read_required_times_out_on_silent_peer() {
        let (_client, server) = duplex(64);
        let mut server = BufReader::new(server);
        match FrameCodec::HANDSHAKE
            .read_required::<_, Message>(&mut server, Duration::from_millis(50))
            .await
        {
            Err(LabyrinthError::Io(error)) => assert_eq!(error.kind(), io::ErrorKind::TimedOut),
            other => panic!("expected timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_required_reports_eof_as_error() {
        let mut input = reader(b"");
        match FrameCodec::HANDSHAKE
            .read_required::<_, Message>(&mut input, Duration::from_secs(1))
            .await
        {
            Err(LabyrinthError::Io(error)) => {
                assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof)
            }
            other => panic!("expected UnexpectedEof, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn frame_reader_survives_cancellation_mid_frame() {
        let (mut client, server) = duplex(64);
        let mut frames = FrameReader::new(BufReader::new(server), FrameCodec::CONTROL);

        client.write_all(b"\"Pi").await.unwrap();
        // Lose the race against a timer while half a frame is buffered.
        let cancelled =
            tokio::time::timeout(Duration::from_millis(20), frames.next::<Message>()).await;
        assert!(cancelled.is_err());

        client.write_all(b"ng\"\n").await.unwrap();
        assert!(matches!(
            frames.next::<Message>().await.unwrap(),
            Some(Message::Ping)
        ));
    }

    #[tokio::test]
    async fn frame_reader_reports_oversize_across_partial_reads() {
        let (mut client, server) = duplex(8);
        let mut frames = FrameReader::new(BufReader::new(server), FrameCodec::new(10));
        let writer = tokio::spawn(async move {
            let _ = client.write_all(&[b'a'; 64]).await;
        });
        assert!(matches!(
            frames.next::<Message>().await,
            Err(LabyrinthError::FrameTooLarge { limit: 10 })
        ));
        drop(frames);
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn large_binary_payload_round_trips_under_control_limit() {
        let data: Vec<u8> = (0..=255u8).cycle().take(512 * 1024).collect();
        let (mut client, server) = duplex(64 * 1024);
        let sent = data.clone();
        let writer = tokio::spawn(async move {
            write_frame(
                &mut client,
                &Message::LinuxElfExecutionRequest {
                    elf_data: sent,
                    args: String::new(),
                },
            )
            .await
            .unwrap();
        });
        let mut server = BufReader::new(server);
        match read_frame::<_, Message>(&mut server).await.unwrap() {
            Some(Message::LinuxElfExecutionRequest { elf_data, .. }) => assert_eq!(elf_data, data),
            other => panic!("unexpected {other:?}"),
        }
        writer.await.unwrap();
    }
}
