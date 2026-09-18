use std::io::{self, Write};

use serde::Serialize;

use crate::envelope::{PROTOCOL, VERSION, decode_envelope};
use crate::{CodecError, Envelope, MAX_FRAME_BODY_BYTES};

/// Result of consuming a single input fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeStatus {
    /// More bytes are required to complete the one allowed frame.
    Pending {
        /// Bytes consumed from this fragment.
        consumed: usize,
    },
    /// One envelope completed; any suffix remains with the caller.
    Complete {
        /// Structurally validated envelope.
        envelope: Envelope,
        /// Bytes consumed from this fragment.
        consumed: usize,
    },
}

/// Incremental decoder for one length-prefixed envelope.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    header: [u8; 4],
    header_len: usize,
    expected_body_len: Option<usize>,
    body: Vec<u8>,
    complete: bool,
}

impl FrameDecoder {
    /// Creates an empty one-frame decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            header: [0; 4],
            header_len: 0,
            expected_body_len: None,
            body: Vec::new(),
            complete: false,
        }
    }

    /// Consumes at most one frame from `input`.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`CodecError`] for an invalid declared length or a
    /// complete body that violates UTF-8, depth, JSON, or envelope rules.
    pub fn push(&mut self, input: &[u8]) -> Result<DecodeStatus, CodecError> {
        if self.complete {
            return Err(CodecError::InvalidEnvelope);
        }
        let mut consumed = 0_usize;
        while self.header_len < self.header.len() && consumed < input.len() {
            let Some(source) = input.get(consumed).copied() else {
                break;
            };
            let Some(destination) = self.header.get_mut(self.header_len) else {
                break;
            };
            *destination = source;
            self.header_len = self.header_len.saturating_add(1);
            consumed = consumed.saturating_add(1);
        }

        if self.header_len < self.header.len() {
            return Ok(DecodeStatus::Pending { consumed });
        }
        if self.expected_body_len.is_none() {
            let declared = usize::try_from(u32::from_be_bytes(self.header))
                .map_err(|_| CodecError::FrameTooLarge)?;
            if declared == 0 {
                return Err(CodecError::EmptyFrame);
            }
            if declared > MAX_FRAME_BODY_BYTES {
                return Err(CodecError::FrameTooLarge);
            }
            self.body
                .try_reserve_exact(declared)
                .map_err(|_| CodecError::FrameTooLarge)?;
            self.expected_body_len = Some(declared);
        }

        let expected = self.expected_body_len.ok_or(CodecError::InvalidEnvelope)?;
        let needed = expected.saturating_sub(self.body.len());
        let available = input.len().saturating_sub(consumed);
        let take = needed.min(available);
        let end = consumed.saturating_add(take);
        let fragment = input
            .get(consumed..end)
            .ok_or(CodecError::InvalidEnvelope)?;
        self.body.extend_from_slice(fragment);
        consumed = end;

        if self.body.len() != expected {
            return Ok(DecodeStatus::Pending { consumed });
        }
        let envelope = decode_envelope(&self.body)?;
        self.complete = true;
        Ok(DecodeStatus::Complete { envelope, consumed })
    }

    /// Classifies terminal input as clean boundary EOF or truncation.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::TruncatedHeader`] or
    /// [`CodecError::TruncatedBody`] when a partial frame is buffered.
    pub fn end_of_input(&self) -> Result<(), CodecError> {
        if self.complete || self.header_len == 0 {
            return Ok(());
        }
        if self.header_len < self.header.len() {
            return Err(CodecError::TruncatedHeader {
                received: self.header_len,
            });
        }
        let expected = self.expected_body_len.ok_or(CodecError::InvalidEnvelope)?;
        Err(CodecError::TruncatedBody {
            expected,
            received: self.body.len(),
        })
    }

    /// Returns body bytes retained by this decoder.
    #[must_use]
    pub const fn buffered_body_bytes(&self) -> usize {
        self.body.len()
    }
}

#[derive(Serialize)]
struct EncodedEnvelope<'a> {
    protocol: &'static str,
    version: i64,
    kind: crate::EnvelopeKind,
    correlation: String,
    #[serde(rename = "type")]
    message_type: &'a str,
    payload: &'a serde_json::value::RawValue,
}

/// Encodes one complete bounded frame without exposing partial output.
///
/// # Errors
///
/// Returns [`CodecError::FrameTooLarge`] when the final body exceeds the
/// fixed budget, or [`CodecError::InvalidEnvelope`] on serialization failure.
pub fn encode_frame(envelope: &Envelope) -> Result<Vec<u8>, CodecError> {
    // Raw input size is checked before serialization/depth work. The payload
    // may fit alone while envelope metadata makes the final body too large.
    if envelope.payload_json().len() > MAX_FRAME_BODY_BYTES {
        return Err(CodecError::FrameTooLarge);
    }
    let wire = EncodedEnvelope {
        protocol: PROTOCOL,
        version: VERSION,
        kind: envelope.kind(),
        correlation: format!("{:016x}", envelope.correlation()),
        message_type: envelope.message_type(),
        payload: envelope.payload(),
    };
    let mut writer = CappedWriter::new(MAX_FRAME_BODY_BYTES);
    if serde_json::to_writer(&mut writer, &wire).is_err() {
        return if writer.exceeded {
            Err(CodecError::FrameTooLarge)
        } else {
            Err(CodecError::InvalidEnvelope)
        };
    }
    let body = writer.into_inner();
    let body_len = u32::try_from(body.len()).map_err(|_| CodecError::FrameTooLarge)?;
    let capacity = body.len().checked_add(4).ok_or(CodecError::FrameTooLarge)?;
    let mut frame = Vec::with_capacity(capacity);
    frame.extend_from_slice(&body_len.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

struct CappedWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl CappedWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            exceeded: false,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for CappedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(new_len) = self.bytes.len().checked_add(buffer.len()) else {
            self.exceeded = true;
            return Err(io::Error::other("bounded JSON output exceeded"));
        };
        if new_len > self.limit {
            self.exceeded = true;
            return Err(io::Error::other("bounded JSON output exceeded"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
