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
    /// The outside call the worker is to be in from now on (a voice
    /// channel id of the service), carrying the room's call into it, and
    /// the line it shows there about who is on the room's side; `call:
    /// None` leaves. It replaces the last one whole, and the host sends it
    /// again whenever the worker restarts. The worker answers with
    /// [`FromWorker::Calls`].
    Call {
        call: Option<String>,
        #[serde(default)]
        status: Option<String>,
    },
    /// One Opus frame (48 kHz, 20 ms) the room's call heard from `speaker`
    /// (an id stable while they speak), for the outside call the worker is
    /// in. Several speakers at once are mixed by the worker; the host never
    /// sends a frame the outside call itself said.
    Audio {
        speaker: String,
        opus: Bytes,
    },
}

/// Who is in one outside call (a voice channel of the service).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideCall {
    /// The service's id of the call: its voice channel.
    pub call: String,
    /// Who is in it, the worker's own account left out, sorted by account.
    pub people: Vec<OutsidePerson>,
}

/// One person in an outside call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsidePerson {
    /// Their account on the service.
    pub account: String,
    /// The name the service shows for them there.
    pub name: String,
    /// Where the service serves their avatar, if they have one.
    #[serde(default)]
    pub avatar_url: Option<String>,
    /// Muted, by themselves or by the service's moderators: nothing of
    /// theirs is heard.
    #[serde(default)]
    pub muted: bool,
}

/// The worker's own place in the outside calls.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "link", rename_all = "kebab-case")]
pub enum CallLink {
    /// In no call.
    #[default]
    Idle,
    /// Joining `call`.
    Joining { call: String },
    /// In `call`, carrying audio both ways.
    Joined { call: String },
    /// The service will not let it into `call`, for `reason` (a missing
    /// permission names it): said as it is, never retried until asked
    /// again.
    Refused { call: String, reason: String },
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
    /// The service's calls as the worker sees them now, and its own place
    /// in them: whenever either changes, and once after [`ToWorker::Call`].
    /// Replaces the last report whole; live state, never logged.
    Calls {
        calls: Vec<OutsideCall>,
        link: CallLink,
    },
    /// One Opus frame (48 kHz, 20 ms) `speaker` (their account) said in the
    /// outside call the worker is in, as the service delivered it.
    Audio {
        speaker: String,
        opus: Bytes,
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
