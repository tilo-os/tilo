//! The typed, versioned messages between a host and a connector worker.

use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize as _;

/// Bumped whenever a message changes shape; a worker answering another
/// version is stopped.
pub const PROTOCOL_VERSION: u32 = 1;
/// Field of a queued op naming content-store blobs to publish at a public URL.
pub const PUBLISH_FIELD: &str = "publish";
/// Field of a queued op naming content-store blobs the worker needs the bytes of.
pub const ATTACH_FIELD: &str = "attach";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ToWorker {
    Start {
        version: u32,
        #[serde(default)]
        checkpoint: Option<serde_json::Value>,
    },
    Perform {
        request: u64,
        op: serde_json::Value,
    },
    Blob {
        blake3: String,
        bytes: Bytes,
    },
    Secret {
        purpose: String,
        item: String,
        #[serde(default)]
        bytes: Option<Bytes>,
    },
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum FromWorker {
    Ready {
        version: u32,
    },
    Result {
        request: u64,
        outcome: Outcome,
    },
    Event {
        source: String,
        id: String,
        at_ms: i64,
        payload: serde_json::Map<String, serde_json::Value>,
    },
    Blob {
        bytes: Bytes,
    },
    NeedSecret {
        purpose: String,
        item: String,
    },
    StoreSecret {
        purpose: String,
        item: String,
        bytes: Bytes,
    },
    Checkpoint {
        state: serde_json::Value,
    },
    Status {
        healthy: bool,
        message: String,
    },
    /// How far behind the worker is, every few seconds, for the host's
    /// metrics file: ops in flight or waiting, the age of the oldest of
    /// them (0 when idle), and how often a rate limit made one wait.
    Metrics {
        queue_depth: u64,
        lag_seconds: f64,
        rate_limited_total: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    Done {
        result: serde_json::Value,
    },
    /// The service refused: dead-letter, never retry.
    Refused {
        reason: String,
    },
    /// The service is unavailable: retry with backoff.
    Unavailable {
        reason: String,
    },
}

/// Bytes, base64 on the wire. Zeroed when dropped, because the same type
/// carries tokens; `Debug` prints only the length.
#[derive(Clone, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Drop for Bytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bytes({} bytes)", self.0.len())
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map(Bytes)
            .map_err(serde::de::Error::custom)
    }
}
