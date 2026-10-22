//! One JSON message per line. A line longer than [`MAX_FRAME_BYTES`] is an
//! error: a 32 MiB attachment is about 43 MiB of base64, and nothing larger
//! is ever sent.

use std::io::{BufRead, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

pub const MAX_FRAME_BYTES: usize = 48 * 1024 * 1024;

fn decode<T: DeserializeOwned>(line: Option<Vec<u8>>) -> std::io::Result<Option<T>> {
    line.map(|line| {
        serde_json::from_slice(&line)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
    .transpose()
}

/// Write `message` as one line and flush it.
///
/// # Errors
/// The stream failed, or `message` does not serialize.
pub fn write_frame<W: Write, T: Serialize>(out: &mut W, message: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

/// The next message, or `None` at the end of the stream.
pub fn read_frame<R: BufRead, T: DeserializeOwned>(input: &mut R) -> std::io::Result<Option<T>> {
    decode(wire_frame::read_line(input, MAX_FRAME_BYTES)?)
}

/// [`write_frame`] over tokio.
///
/// # Errors
/// As [`write_frame`].
pub async fn write_frame_async<W, T>(out: &mut W, message: &T) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin + Send,
    T: Serialize + Sync,
{
    use tokio::io::AsyncWriteExt as _;
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    out.write_all(&line).await?;
    out.flush().await
}

/// [`read_frame`] over tokio.
///
/// # Errors
/// As [`read_frame`].
pub async fn read_frame_async<R, T>(input: &mut R) -> std::io::Result<Option<T>>
where
    R: tokio::io::AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    decode(wire_frame::read_line_async(input, MAX_FRAME_BYTES).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Bytes, FromWorker, Outcome, ToWorker};
    use std::io::BufReader;

    #[test]
    fn frames_round_trip_one_per_line() {
        let mut out = Vec::new();
        write_frame(
            &mut out,
            &ToWorker::Perform {
                request: 7,
                op: serde_json::json!({"op": "send"}),
            },
        )
        .unwrap();
        write_frame(
            &mut out,
            &ToWorker::Blob {
                blake3: "ab".into(),
                bytes: Bytes(vec![0, 1, 255]),
            },
        )
        .unwrap();
        assert_eq!(std::io::BufRead::lines(out.as_slice()).count(), 2);
        assert_eq!(out.last(), Some(&b'\n'));
        let mut input = BufReader::new(out.as_slice());
        let first: ToWorker = read_frame(&mut input).unwrap().unwrap();
        assert!(matches!(first, ToWorker::Perform { request: 7, .. }));
        let second: ToWorker = read_frame(&mut input).unwrap().unwrap();
        let ToWorker::Blob { bytes, .. } = second else {
            panic!("a blob")
        };
        assert_eq!(bytes.0, vec![0, 1, 255]);
        assert!(
            read_frame::<_, ToWorker>(&mut input).unwrap().is_none(),
            "end of stream"
        );
    }

    #[test]
    fn an_oversized_frame_is_an_error_not_an_allocation() {
        let line = vec![b'x'; MAX_FRAME_BYTES + 2];
        let mut input = BufReader::new(line.as_slice());
        assert!(read_frame::<_, FromWorker>(&mut input).is_err());
    }

    #[test]
    fn outcomes_are_tagged() {
        let value = serde_json::to_value(Outcome::Refused {
            reason: "Missing Permissions".into(),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"outcome": "refused", "reason": "Missing Permissions"})
        );
    }

    #[test]
    fn secret_bytes_never_print() {
        let secret = ToWorker::Secret {
            purpose: "bot-token".into(),
            item: "default".into(),
            bytes: Some(Bytes(b"hunter2".to_vec())),
        };
        assert!(!format!("{secret:?}").contains("hunter2"));
    }

    #[tokio::test]
    async fn async_frames_match_sync_frames() {
        let mut out = Vec::new();
        write_frame_async(&mut out, &FromWorker::Ready { version: 1 })
            .await
            .unwrap();
        let mut input = tokio::io::BufReader::new(out.as_slice());
        let ready: FromWorker = read_frame_async(&mut input).await.unwrap().unwrap();
        assert!(matches!(ready, FromWorker::Ready { version: 1 }));
    }
}
