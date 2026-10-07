//! Shared worker wire types. The compositor never links the native decoder.
//! Replies carry no Debug implementation: recorded words must not reach logs.
use serde::{Deserialize, Serialize};

mod cleanup;
pub use cleanup::clean_final;

pub const SAMPLE_RATE: usize = 16_000;
pub const MAX_SAMPLES: usize = SAMPLE_RATE * 120;
pub const MAX_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_CONFIG_BYTES: usize = 32 * 1024;
pub const FRAME: wire_frame::Framing =
    wire_frame::Framing::little_endian(64 * 1024 + 9).reject_empty();
/// Status contains no audio, transcript, path or model-controlled message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechError {
    ModelUnavailable,
    ModelReceipt,
    UnsupportedLanguage,
    Protocol,
    AudioTooLong,
    TextTooLong,
    Recognition,
    WorkerStopped,
}

/// Bounded scheduling chosen by the image worker's decoder implementation.
/// Full-context decoding retains its measured opening-word context; genuine
/// incremental decoding can expose new tokens without redecoding that prefix.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftPolicy {
    #[default]
    FullContext,
    Incremental,
}
impl DraftPolicy {
    pub const fn initial_delay(&self) -> std::time::Duration {
        match self {
            Self::FullContext => std::time::Duration::from_secs(2),
            Self::Incremental => std::time::Duration::from_millis(500),
        }
    }
    pub const fn interval(&self) -> std::time::Duration {
        match self {
            Self::FullContext => std::time::Duration::from_secs(1),
            Self::Incremental => std::time::Duration::from_millis(500),
        }
    }
    fn is_default(&self) -> bool {
        *self == Self::FullContext
    }
}

/// A reply to one monotonic request sequence on one private worker pipe.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SpeechReply {
    Ready {
        sequence: u64,
        model: String,
        revision: String,
        #[serde(default, skip_serializing_if = "DraftPolicy::is_default")]
        draft_policy: DraftPolicy,
    },
    Draft {
        sequence: u64,
        text: String,
    },
    Done {
        sequence: u64,
        text: String,
    },
    Error {
        sequence: u64,
        error: SpeechError,
    },
}

#[derive(Serialize, Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
#[serde(deny_unknown_fields)]
pub struct SpeechConfig {
    pub language: String,
    /// Accepted spellings only, never instructions or transcript replacements.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vocabulary: Vec<String>,
}

/// Requests are ordered on a private byte pipe. Audio frames contain S16LE;
/// control frames never accept model paths, commands, URLs or arbitrary prompts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Configure = 0,
    Audio = 1,
    Draft = 2,
    Finish = 3,
}

pub fn request(kind: RequestKind, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(payload.len().saturating_add(9));
    bytes.push(kind as u8);
    bytes.extend(sequence.to_le_bytes());
    bytes.extend(payload);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_wire_preserves_legacy_context_and_rejects_unbounded_policy() {
        let legacy = br#"{"kind":"ready","sequence":1,"model":"fixture","revision":"1"}"#;
        let ready: SpeechReply = serde_json::from_slice(legacy).unwrap();
        assert!(matches!(
            ready,
            SpeechReply::Ready {
                draft_policy: DraftPolicy::FullContext,
                ..
            }
        ));
        assert_eq!(serde_json::to_vec(&ready).unwrap(), legacy);
        let incremental = br#"{"kind":"ready","sequence":1,"model":"fixture","revision":"1","draft_policy":"incremental"}"#;
        let ready: SpeechReply = serde_json::from_slice(incremental).unwrap();
        assert!(matches!(
            ready,
            SpeechReply::Ready {
                draft_policy: DraftPolicy::Incremental,
                ..
            }
        ));
        assert_eq!(serde_json::to_vec(&ready).unwrap(), incremental);
        let arbitrary = br#"{"kind":"ready","sequence":1,"model":"fixture","revision":"1","draft_policy":{"interval_ms":0}}"#;
        assert!(serde_json::from_slice::<SpeechReply>(arbitrary).is_err());
    }
}
