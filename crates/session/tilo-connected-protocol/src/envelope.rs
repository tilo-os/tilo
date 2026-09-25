use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::{MAX_FRAME_BODY_BYTES, MAX_JSON_DEPTH};

pub(crate) const PROTOCOL: &str = "tilo.connected";
pub(crate) const VERSION: i64 = 1;
const VERSION_TEXT: &str = "1";

/// The structural direction asserted by an envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvelopeKind {
    /// Candidate client request data.
    Request,
    /// Candidate peer response data.
    Response,
}

/// A structurally validated envelope with an opaque JSON-object payload.
#[derive(Clone, Debug)]
pub struct Envelope {
    kind: EnvelopeKind,
    correlation: u64,
    message_type: String,
    payload: Box<RawValue>,
}

impl PartialEq for Envelope {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.correlation == other.correlation
            && self.message_type == other.message_type
            && self.payload.get() == other.payload.get()
    }
}

impl Eq for Envelope {}

impl Envelope {
    /// Builds request data after applying the complete envelope validation.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`CodecError`] when correlation, type, payload size,
    /// depth, JSON syntax, or payload shape violates the candidate schema.
    pub fn request(
        correlation: u64,
        message_type: &str,
        payload_json: &str,
    ) -> Result<Self, CodecError> {
        Self::new(
            EnvelopeKind::Request,
            correlation,
            message_type,
            payload_json,
        )
    }

    /// Builds response data after applying the complete envelope validation.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`CodecError`] when correlation, type, payload size,
    /// depth, JSON syntax, or payload shape violates the candidate schema.
    pub fn response(
        correlation: u64,
        message_type: &str,
        payload_json: &str,
    ) -> Result<Self, CodecError> {
        Self::new(
            EnvelopeKind::Response,
            correlation,
            message_type,
            payload_json,
        )
    }

    fn new(
        kind: EnvelopeKind,
        correlation: u64,
        message_type: &str,
        payload_json: &str,
    ) -> Result<Self, CodecError> {
        // This cheap bound deliberately precedes syntax and depth work.
        if payload_json.len() > MAX_FRAME_BODY_BYTES {
            return Err(CodecError::FrameTooLarge);
        }
        validate_correlation(correlation)?;
        validate_message_type(message_type)?;
        check_json_depth(payload_json.as_bytes(), MAX_JSON_DEPTH.saturating_sub(1))?;
        let payload = parse_raw_json(payload_json)?;
        if !is_json_object(payload.get()) {
            return Err(CodecError::InvalidEnvelope);
        }
        Ok(Self {
            kind,
            correlation,
            message_type: message_type.to_owned(),
            payload,
        })
    }

    /// Returns the structural envelope direction.
    #[must_use]
    pub const fn kind(&self) -> EnvelopeKind {
        self.kind
    }

    /// Returns the nonzero, stream-local correlation number.
    #[must_use]
    pub const fn correlation(&self) -> u64 {
        self.correlation
    }

    /// Returns the validated candidate message type token.
    #[must_use]
    pub fn message_type(&self) -> &str {
        &self.message_type
    }

    /// Returns the retained, structurally unvalidated domain payload.
    #[must_use]
    pub fn payload_json(&self) -> &str {
        self.payload.get()
    }

    pub(crate) const fn payload(&self) -> &RawValue {
        &self.payload
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEnvelope {
    protocol: String,
    version: Box<RawValue>,
    kind: String,
    correlation: String,
    #[serde(rename = "type")]
    message_type: String,
    payload: Box<RawValue>,
}

pub(crate) fn decode_envelope(body: &[u8]) -> Result<Envelope, CodecError> {
    let text = std::str::from_utf8(body).map_err(|error| CodecError::InvalidUtf8 {
        offset: error.valid_up_to(),
    })?;
    check_json_depth(body, MAX_JSON_DEPTH)?;
    // Parse once as an opaque value to separate JSON syntax/trailing-token
    // failures from strict envelope-shape failures such as duplicate fields.
    let syntax = parse_raw_json(text)?;
    if !is_json_object(syntax.get()) {
        return Err(CodecError::InvalidEnvelope);
    }
    let wire: WireEnvelope = serde_json::from_str(text).map_err(|_| CodecError::InvalidEnvelope)?;

    if wire.protocol != PROTOCOL {
        return Err(CodecError::InvalidEnvelope);
    }
    if !is_json_integer(wire.version.get()) {
        return Err(CodecError::InvalidEnvelope);
    }
    if wire.version.get() != VERSION_TEXT {
        return Err(CodecError::UnsupportedVersion);
    }
    let kind = match wire.kind.as_str() {
        "request" => EnvelopeKind::Request,
        "response" => EnvelopeKind::Response,
        _ => return Err(CodecError::InvalidEnvelope),
    };
    let correlation = parse_correlation(&wire.correlation)?;
    validate_message_type(&wire.message_type)?;
    if !is_json_object(wire.payload.get()) {
        return Err(CodecError::InvalidEnvelope);
    }

    Ok(Envelope {
        kind,
        correlation,
        message_type: wire.message_type,
        payload: wire.payload,
    })
}

fn parse_raw_json(text: &str) -> Result<Box<RawValue>, CodecError> {
    serde_json::from_str(text).map_err(|error| CodecError::InvalidJson {
        offset: json_error_offset(text, &error),
    })
}

fn json_error_offset(text: &str, error: &serde_json::Error) -> usize {
    if error.is_eof() {
        return text.len();
    }
    let line_offset = text
        .split_inclusive('\n')
        .take(error.line().saturating_sub(1))
        .fold(0_usize, |offset, line| offset.saturating_add(line.len()));
    line_offset.saturating_add(error.column().saturating_sub(1))
}

fn is_json_object(raw: &str) -> bool {
    raw.as_bytes()
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'{')
}

fn is_json_integer(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let digits = if bytes.first() == Some(&b'-') {
        bytes.get(1..).unwrap_or_default()
    } else {
        bytes
    };
    !digits.is_empty() && digits.iter().all(u8::is_ascii_digit)
}

fn parse_correlation(value: &str) -> Result<u64, CodecError> {
    if value.len() != 16
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(CodecError::InvalidEnvelope);
    }
    let correlation = u64::from_str_radix(value, 16).map_err(|_| CodecError::InvalidEnvelope)?;
    validate_correlation(correlation)?;
    Ok(correlation)
}

fn validate_correlation(correlation: u64) -> Result<(), CodecError> {
    if correlation == 0 {
        Err(CodecError::InvalidEnvelope)
    } else {
        Ok(())
    }
}

fn validate_message_type(message_type: &str) -> Result<(), CodecError> {
    let bytes = message_type.as_bytes();
    let valid = (1..=64).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.iter().skip(1).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(CodecError::InvalidEnvelope)
    }
}

pub(crate) fn check_json_depth(input: &[u8], limit: usize) -> Result<(), CodecError> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in input.iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > limit {
                    return Err(CodecError::DepthExceeded { offset });
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Bounded codec failures. Errors expose categories and byte offsets only.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CodecError {
    /// A zero-length body is forbidden.
    #[error("empty frame")]
    EmptyFrame,
    /// A declared or encoded body exceeds the fixed frame budget.
    #[error("frame too large")]
    FrameTooLarge,
    /// End-of-input occurred inside the four-byte prefix.
    #[error("truncated frame header after {received} bytes")]
    TruncatedHeader {
        /// Prefix bytes received.
        received: usize,
    },
    /// End-of-input occurred before the declared body completed.
    #[error("truncated frame body: expected {expected} bytes, received {received}")]
    TruncatedBody {
        /// Declared body size.
        expected: usize,
        /// Body bytes received.
        received: usize,
    },
    /// The body is not UTF-8.
    #[error("invalid UTF-8 at byte {offset}")]
    InvalidUtf8 {
        /// First invalid byte offset.
        offset: usize,
    },
    /// The body is not exactly one JSON value.
    #[error("invalid JSON at byte {offset}")]
    InvalidJson {
        /// Parser byte offset.
        offset: usize,
    },
    /// JSON nesting crossed the fixed limit.
    #[error("JSON depth exceeded at byte {offset}")]
    DepthExceeded {
        /// First byte that crosses the limit.
        offset: usize,
    },
    /// Envelope fields or tokens violate the candidate schema.
    #[error("invalid envelope")]
    InvalidEnvelope,
    /// The envelope names a protocol version this codec does not accept.
    #[error("unsupported protocol version")]
    UnsupportedVersion,
    /// A standalone native-string representation is invalid.
    #[error("invalid native string at byte {offset}")]
    InvalidNativeString {
        /// Byte offset in the representation, when known.
        offset: usize,
    },
}
