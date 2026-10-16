use std::io::{self, Cursor, Read};

use super::*;

const LE: Framing = Framing::little_endian(16);
const BE: Framing = Framing::big_endian(16);

/// Hands out one byte per read and an `Interrupted` error before each, so
/// every read path has to cope with short and interrupted reads.
struct Trickle {
    bytes: Vec<u8>,
    at: usize,
    interrupt: bool,
}

impl Trickle {
    fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            at: 0,
            interrupt: true,
        }
    }
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.interrupt = !self.interrupt;
        if !self.interrupt {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let Some(byte) = self.bytes.get(self.at) else {
            return Ok(0);
        };
        if buf.is_empty() {
            return Ok(0);
        }
        buf[0] = *byte;
        self.at = self.at.saturating_add(1);
        Ok(1)
    }
}

#[test]
fn frames_encode_with_an_explicit_prefix_and_read_back_completely() {
    // Phase `prefix_byte_order_is_explicit`.
    {
        assert_eq!(LE.encode(&[7, 8, 9]).unwrap(), [3, 0, 0, 0, 7, 8, 9]);
        assert_eq!(BE.encode(&[7, 8, 9]).unwrap(), [0, 0, 0, 3, 7, 8, 9]);
        let big = Framing::little_endian(usize::MAX);
        assert_eq!(big.encode_prefix(0x0102_0304).unwrap(), [4, 3, 2, 1]);
        assert_eq!(
            Framing::big_endian(usize::MAX)
                .encode_prefix(0x0102_0304)
                .unwrap(),
            [1, 2, 3, 4]
        );
    }
    // Phase `write_matches_encode_and_reads_back`.
    {
        for framing in [LE, BE] {
            let mut wire = Vec::new();
            framing.write(&mut wire, b"one").unwrap();
            framing.write(&mut wire, b"").unwrap();
            framing.write(&mut wire, b"three").unwrap();
            let mut expected = framing.encode(b"one").unwrap();
            expected.extend(framing.encode(b"").unwrap());
            expected.extend(framing.encode(b"three").unwrap());
            assert_eq!(wire, expected);

            let mut reader = Cursor::new(wire);
            assert_eq!(framing.read(&mut reader).unwrap().unwrap(), b"one");
            assert_eq!(framing.read(&mut reader).unwrap().unwrap(), b"");
            assert_eq!(framing.read(&mut reader).unwrap().unwrap(), b"three");
            assert!(framing.read(&mut reader).unwrap().is_none());
        }
    }
    // Phase `short_and_interrupted_reads_are_completed`.
    {
        let wire = LE.encode(b"hello").unwrap();
        let mut reader = Trickle::new(&wire);
        assert_eq!(LE.read(&mut reader).unwrap().unwrap(), b"hello");
        assert!(LE.read(&mut reader).unwrap().is_none());
    }
    // Phase `read_len_leaves_the_payload_to_the_caller`.
    {
        let mut wire = Cursor::new(BE.encode(b"abc").unwrap());
        assert_eq!(BE.read_len(&mut wire).unwrap(), Some(3));
        assert_eq!(wire.position(), 4);
        assert_eq!(BE.read_len(&mut Cursor::new([])).unwrap(), None);
    }
    // Phase `read_into_reuses_the_buffer`.
    {
        let mut wire = LE.encode(b"longer").unwrap();
        wire.extend(LE.encode(b"ab").unwrap());
        let mut reader = Cursor::new(wire);
        let mut buf = Vec::new();
        assert!(LE.read_into(&mut reader, &mut buf).unwrap());
        assert_eq!(buf, b"longer");
        let capacity = buf.capacity();
        assert!(LE.read_into(&mut reader, &mut buf).unwrap());
        assert_eq!(buf, b"ab");
        assert_eq!(buf.capacity(), capacity);
        assert!(!LE.read_into(&mut reader, &mut buf).unwrap());
        assert!(buf.is_empty());
    }
    // Phase `frame_reader_yields_each_payload`.
    {
        let mut wire = BE.encode(b"first").unwrap();
        wire.extend(BE.encode(b"2nd").unwrap());
        let mut reader = FrameReader::new(Cursor::new(wire), BE);
        assert_eq!(reader.read_frame().unwrap().unwrap(), b"first");
        assert_eq!(reader.read_frame().unwrap().unwrap(), b"2nd");
        assert!(reader.read_frame().unwrap().is_none());
        assert_eq!(reader.framing(), BE);
    }
}

#[test]
fn frame_limits_truncation_and_empty_policy_are_enforced() {
    // Phase `eof_inside_a_frame_is_truncation`.
    {
        for wire in [&[5_u8][..], &[5, 0, 0], &[5, 0, 0, 0, b'h', b'i']] {
            let error = LE.read(&mut Cursor::new(wire)).unwrap_err();
            assert!(matches!(error, FrameError::Truncated), "{wire:?}: {error}");
            assert_eq!(io::Error::from(error).kind(), io::ErrorKind::UnexpectedEof);
        }
    }
    // Phase `oversized_prefix_is_rejected_before_the_payload_is_read`.
    {
        let mut wire = Cursor::new(vec![17, 0, 0, 0, 1, 2, 3]);
        let error = LE.read(&mut wire).unwrap_err();
        assert!(matches!(error, FrameError::TooLarge(17)));
        assert_eq!(wire.position(), 4);
        assert_eq!(io::Error::from(error).kind(), io::ErrorKind::InvalidData);
        assert!(matches!(
            LE.read(&mut Cursor::new([16, 0, 0, 0])),
            Err(FrameError::Truncated)
        ));
    }
    // Phase `oversized_payload_is_refused_without_writing`.
    {
        let mut wire = Vec::new();
        let error = LE.write(&mut wire, &[0; 17]).unwrap_err();
        assert!(matches!(error, FrameError::PayloadTooLarge(17)));
        assert!(wire.is_empty());
        assert_eq!(io::Error::from(error).kind(), io::ErrorKind::InvalidInput);
        LE.write(&mut wire, &[0; 16]).unwrap();
        assert_eq!(wire.len(), 20);
    }
    // Phase `empty_frames_can_be_forbidden`.
    {
        let strict = LE.reject_empty();
        assert!(matches!(
            strict.read(&mut Cursor::new([0, 0, 0, 0])),
            Err(FrameError::EmptyFrame)
        ));
        assert!(matches!(
            strict.write(&mut Vec::new(), b""),
            Err(FrameError::EmptyFrame)
        ));
        assert_eq!(
            LE.read(&mut Cursor::new([0, 0, 0, 0])).unwrap().unwrap(),
            b""
        );
    }
    // Phase `closed_and_decode_errors_convert_to_io_kinds`.
    {
        assert_eq!(
            io::Error::from(FrameError::Closed).kind(),
            io::ErrorKind::UnexpectedEof
        );
        let passthrough = io::Error::from(FrameError::Io(io::ErrorKind::BrokenPipe.into()));
        assert_eq!(passthrough.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(FrameError::EmptyFrame.kind(), io::ErrorKind::InvalidData);
    }
    // Phase `with_max_len_keeps_order_and_empty_policy`.
    {
        let handoff = BE.reject_empty().with_max_len(4);
        assert_eq!(handoff.byte_order(), ByteOrder::BigEndian);
        assert_eq!(handoff.max_len(), 4);
        assert!(matches!(
            handoff.read(&mut Cursor::new([0, 0, 0, 0])),
            Err(FrameError::EmptyFrame)
        ));
        assert!(matches!(
            handoff.read(&mut Cursor::new([0, 0, 0, 5])),
            Err(FrameError::TooLarge(5))
        ));
    }
}

#[test]
fn lines_are_bounded_and_end_with_a_newline() {
    let mut reader = Cursor::new(b"one\n\nfour\nlast".to_vec());
    assert_eq!(
        read_line(&mut reader, 4).unwrap().as_deref(),
        Some(&b"one"[..])
    );
    assert_eq!(
        read_line(&mut reader, 4).unwrap().as_deref(),
        Some(&b""[..])
    );
    assert_eq!(
        read_line(&mut reader, 4).unwrap().as_deref(),
        Some(&b"four"[..])
    );
    assert!(matches!(
        read_line(&mut reader, 4),
        Err(FrameError::Truncated)
    ));
    assert_eq!(read_line(&mut reader, 4).unwrap(), None);

    assert!(matches!(
        read_line(&mut Cursor::new(b"fives\n".to_vec()), 4),
        Err(FrameError::TooLarge(5))
    ));
    // A peer that never sends a newline is cut off at the limit.
    let mut endless = io::BufReader::new(io::repeat(b'a'));
    assert!(matches!(
        read_line(&mut endless, 1024),
        Err(FrameError::TooLarge(1025))
    ));
}

#[cfg(feature = "tokio")]
mod asynchronous {
    use super::*;

    #[tokio::test]
    async fn async_lines_match_blocking_lines() {
        let mut reader = &b"one\nfives\n"[..];
        assert_eq!(
            read_line_async(&mut reader, 4).await.unwrap().as_deref(),
            Some(&b"one"[..])
        );
        assert!(matches!(
            read_line_async(&mut reader, 4).await,
            Err(FrameError::TooLarge(5))
        ));
    }

    #[tokio::test]
    async fn async_framing_matches_blocking_framing_and_limits() {
        // Phase `async_matches_blocking`.
        {
            for framing in [LE, BE] {
                let mut wire = Vec::new();
                framing.write_async(&mut wire, b"hello").await.unwrap();
                framing.write_async(&mut wire, b"").await.unwrap();
                assert_eq!(wire[..9], framing.encode(b"hello").unwrap()[..]);

                let mut reader = wire.as_slice();
                assert_eq!(
                    framing.read_async(&mut reader).await.unwrap().unwrap(),
                    b"hello"
                );
                assert_eq!(framing.read_async(&mut reader).await.unwrap().unwrap(), b"");
                assert!(framing.read_async(&mut reader).await.unwrap().is_none());
            }
        }
        // Phase `async_truncation_and_limits`.
        {
            assert!(matches!(
                LE.read_async(&mut &[5_u8, 0][..]).await,
                Err(FrameError::Truncated)
            ));
            assert!(matches!(
                LE.read_async(&mut &[2_u8, 0, 0, 0, 1][..]).await,
                Err(FrameError::Truncated)
            ));
            assert!(matches!(
                LE.read_async(&mut &[17_u8, 0, 0, 0][..]).await,
                Err(FrameError::TooLarge(17))
            ));
            let mut wire = Vec::new();
            assert!(LE.write_async(&mut wire, &[0; 17]).await.is_err());
            assert!(wire.is_empty());
        }
        // Phase `async_frame_reader_over_a_split_stream`.
        {
            let (mut client, server) = tokio::io::duplex(3);
            let writer = tokio::spawn(async move {
                LE.write_async(&mut client, b"across").await.unwrap();
                LE.write_async(&mut client, b"chunks").await.unwrap();
            });
            let mut reader = FrameReader::new(server, LE);
            assert_eq!(reader.read_frame_async().await.unwrap().unwrap(), b"across");
            assert_eq!(reader.read_frame_async().await.unwrap().unwrap(), b"chunks");
            writer.await.unwrap();
            assert!(reader.read_frame_async().await.unwrap().is_none());
        }
    }

    /// Hands out one byte per poll and is pending in between, like a stream
    /// whose frames arrive across several packets.
    struct Trickle {
        bytes: std::collections::VecDeque<u8>,
        ready: bool,
    }

    impl tokio::io::AsyncRead for Trickle {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if !std::mem::replace(&mut self.ready, false) {
                self.ready = true;
                cx.waker().wake_by_ref();
                return std::task::Poll::Pending;
            }
            if let Some(byte) = self.bytes.pop_front() {
                buf.put_slice(&[byte]);
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A receive loop `select!`s the next frame against other work, so the
    /// read is dropped whenever that work is ready first. The frame read so
    /// far must survive; before, its start was lost and the rest of the
    /// stream decoded as garbage prefixes.
    #[tokio::test]
    async fn a_frame_read_dropped_mid_frame_resumes_where_it_stopped() {
        let framing = Framing::little_endian(64);
        let mut wire = Vec::new();
        for payload in [&b"first frame"[..], b"", b"a second, longer frame"] {
            framing.write_async(&mut wire, payload).await.unwrap();
        }
        let mut reader = FrameReader::new(
            Trickle {
                bytes: wire.into(),
                ready: false,
            },
            framing,
        );
        let mut frames = Vec::new();
        let mut interrupted = 0_u32;
        while frames.len() < 3 {
            tokio::select! {
                biased;
                frame = reader.read_frame_async() => {
                    frames.push(frame.unwrap().unwrap().to_vec());
                }
                () = std::future::ready(()) => interrupted = interrupted.saturating_add(1),
            }
        }
        assert!(interrupted > 20, "the read was dropped mid-frame");
        assert_eq!(
            frames,
            vec![
                b"first frame".to_vec(),
                Vec::new(),
                b"a second, longer frame".to_vec()
            ]
        );
    }
}

#[cfg(feature = "postcard")]
mod values {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    enum Message {
        Ping,
        Data { id: u32, bytes: Vec<u8> },
    }

    #[test]
    fn value_frames_are_prefixed_postcard_and_bounded() {
        // Phase `value_frames_are_prefixed_postcard`.
        {
            let message = Message::Data {
                id: 300,
                bytes: vec![9, 8],
            };
            let framed = LE.encode_value(&message).unwrap();
            assert_eq!(framed, [6, 0, 0, 0, 1, 0xac, 0x02, 2, 9, 8]);
            assert_eq!(
                framed,
                LE.encode(&postcard::to_allocvec(&message).unwrap())
                    .unwrap()
            );
            assert_eq!(BE.encode_value(&Message::Ping).unwrap(), [0, 0, 0, 1, 0]);

            let mut wire = Vec::new();
            LE.write_value(&mut wire, &message).unwrap();
            LE.write_value(&mut wire, &Message::Ping).unwrap();
            let mut reader = Cursor::new(wire.clone());
            assert_eq!(
                LE.read_value::<_, Message>(&mut reader).unwrap(),
                Some(message)
            );
            assert_eq!(
                LE.read_value::<_, Message>(&mut reader).unwrap(),
                Some(Message::Ping)
            );
            assert_eq!(LE.read_value::<_, Message>(&mut reader).unwrap(), None);

            let mut buffered = FrameReader::new(Cursor::new(wire), LE);
            assert!(buffered.read_value::<Message>().unwrap().is_some());
            assert_eq!(
                buffered.read_value::<Message>().unwrap(),
                Some(Message::Ping)
            );
        }
        // Phase `oversized_value_is_refused_and_bad_payload_is_decode_error`.
        {
            let big = Message::Data {
                id: 0,
                bytes: vec![0; 32],
            };
            let mut wire = Vec::new();
            assert!(matches!(
                LE.write_value(&mut wire, &big),
                Err(FrameError::PayloadTooLarge(_))
            ));
            assert!(wire.is_empty());
            let error = LE
                .read_value::<_, Message>(&mut Cursor::new([1, 0, 0, 0, 7]))
                .unwrap_err();
            assert!(matches!(error, FrameError::Decode(_)));
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn async_values_round_trip() {
        let mut wire = Vec::new();
        BE.write_value_async(&mut wire, &Message::Ping)
            .await
            .unwrap();
        assert_eq!(wire, [0, 0, 0, 1, 0]);
        let mut reader = wire.as_slice();
        assert_eq!(
            BE.read_value_async::<_, Message>(&mut reader)
                .await
                .unwrap(),
            Some(Message::Ping)
        );
        let mut frames = FrameReader::new(&[0_u8, 0, 0, 1, 0][..], BE);
        assert_eq!(
            frames.read_value_async::<Message>().await.unwrap(),
            Some(Message::Ping)
        );
    }
}
