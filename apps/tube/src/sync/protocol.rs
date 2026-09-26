//! Grayjay "Sync" wire-protocol facts — reimplemented clean-room from the
//! **open** grayjay-android sources.
//!
//! Every constant below is cited to the exact source file it was read from.
//! This module is *pure*: constants, JSON package models, and the plaintext
//! frame codec — no sockets, no Noise, no gzip. It therefore compiles and is
//! unit-tested on the **default hermetic build**. The networked, encrypted
//! transport lives in [`super::transport`] behind the `grayjay-sync` feature.
//!
//! ## Licensing
//!
//! Grayjay's app code is FUTO Source-First (non-commercial) and the `YouTube`
//! plugin is AGPL-3.0. What lives here is our own Rust: the *facts* of an
//! interoperable wire protocol (opcode integers, Noise pattern strings, framing
//! layout, JSON field names). None of Grayjay's source is copied.
//!
//! ## Source map (grayjay-android, `app/src/main/java/com/futo/platformplayer`)
//!
//! * `sync/internal/Opcode.kt` — top-level frame opcodes + sub-opcode enums.
//! * `sync/GJSyncOpcodes.kt` — the DATA sub-opcodes (subscriptions/history/…).
//! * `sync/internal/ContentEncoding.kt` — Raw/Gzip.
//! * `sync/internal/SyncSocketSession.kt` — framing, handshake, version check,
//!   `MAXIMUM_PACKET_SIZE`/`HEADER_SIZE`, and the `nProtocolName` (pairing).
//! * `sync/internal/SyncService.kt` — `protocolName` (main channel) + port.
//! * `sync/internal/SyncDeviceInfo.kt` — the pairing blob.
//! * `sync/models/SyncSubscriptionsPackage.kt`, `models/Subscription.kt`,
//!   `api/media/models/channels/SerializedChannel.kt`, `api/media/PlatformID.kt`
//!   — the JSON payload shapes.
//! * `sync/SyncSessionData.kt` — the state-exchange handshake payload.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Transport constants
// ---------------------------------------------------------------------------

/// TCP port Grayjay-Android listens on for direct P2P sync.
///
/// Source: `SyncService.kt` — `val listenerPort: Int = 12315`.
pub const ANDROID_LISTEN_PORT: u16 = 12315;

/// Noise handshake pattern for the **main** encrypted channel.
///
/// Source: `SyncService.kt` companion object — `dh="25519"`, `pattern="IK"`,
/// `cipher="ChaChaPoly"`, `hash="BLAKE2b"`, giving
/// `protocolName = "Noise_IK_25519_ChaChaPoly_BLAKE2b"`. IK = the initiator
/// already knows (is paired with) the responder's static public key.
pub const NOISE_PATTERN_MAIN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2b";

/// Noise handshake pattern for the **pairing** sub-handshake that carries the
/// out-of-band pairing code inside the first frame.
///
/// Source: `SyncSocketSession.kt` companion object — `pattern="N"`, giving
/// `nProtocolName = "Noise_N_25519_ChaChaPoly_BLAKE2b"`. N = anonymous
/// initiator → known responder; the pairing code travels as the N payload.
pub const NOISE_PATTERN_PAIRING: &str = "Noise_N_25519_ChaChaPoly_BLAKE2b";

/// Wire-format version we advertise, and the minimum we accept from a peer.
///
/// Source: `SyncSocketSession.performVersionCheck()` — `CURRENT_VERSION = 5`,
/// `MINIMUM_VERSION = 4`. The check happens on the raw socket *before* the Noise
/// handshake: each side writes a 4-byte LE int and reads the peer's.
pub const PROTOCOL_VERSION: i32 = 5;
/// Minimum acceptable peer protocol version. See [`PROTOCOL_VERSION`].
pub const MINIMUM_PROTOCOL_VERSION: i32 = 4;

/// Maximum decrypted packet size. Source: `SyncSocketSession.kt` —
/// `MAXIMUM_PACKET_SIZE = 65535 - 16` (leaving room for the 16-byte AEAD tag).
pub const MAXIMUM_PACKET_SIZE: usize = 65535 - 16;
/// Maximum on-the-wire (encrypted) packet size. Source: `SyncSocketSession.kt` —
/// `MAXIMUM_PACKET_SIZE_ENCRYPTED = MAXIMUM_PACKET_SIZE + 16`.
pub const MAXIMUM_PACKET_SIZE_ENCRYPTED: usize = MAXIMUM_PACKET_SIZE + 16;
/// Decrypted-frame header size in bytes: `size:u32 + opcode:u8 + subOpcode:u8 +
/// contentEncoding:u8`. Source: `SyncSocketSession.kt` — `HEADER_SIZE = 7`.
pub const HEADER_SIZE: usize = 7;

// ---------------------------------------------------------------------------
// Opcodes — top-level frame opcode (first byte after the size field)
// ---------------------------------------------------------------------------

/// Top-level frame opcodes. Source: `sync/internal/Opcode.kt` `enum class Opcode`.
pub mod opcode {
    pub const PING: u8 = 0;
    pub const PONG: u8 = 1;
    pub const NOTIFY: u8 = 2;
    pub const STREAM: u8 = 3;
    pub const DATA: u8 = 4;
    pub const REQUEST: u8 = 5;
    pub const RESPONSE: u8 = 6;
    pub const RELAY: u8 = 7;
}

/// NOTIFY sub-opcodes (authorization handshake). Source: `Opcode.kt`
/// `enum class NotifyOpcode`.
pub mod notify {
    pub const AUTHORIZED: u8 = 0;
    pub const UNAUTHORIZED: u8 = 1;
    pub const CONNECTION_INFO: u8 = 2;
}

/// STREAM sub-opcodes (chunked payloads larger than one packet). Source:
/// `Opcode.kt` `enum class StreamOpcode`.
pub mod stream {
    pub const START: u8 = 0;
    pub const DATA: u8 = 1;
    pub const END: u8 = 2;
}

// ---------------------------------------------------------------------------
// DATA sub-opcodes — the actual sync "channels"
// ---------------------------------------------------------------------------

/// DATA sub-opcodes, carried as the `subOpcode` byte of an [`opcode::DATA`]
/// frame. Source: `sync/GJSyncOpcodes.kt` (all `UByte`).
pub mod sync_opcode {
    /// `sendToDevices = 101` — open a URL on the remote device.
    pub const SEND_TO_DEVICES: u8 = 101;
    /// `syncStateExchange = 150` — [`super::SyncSessionData`] JSON; the peer
    /// replies with the packages below covering everything newer than the
    /// timestamps it carries.
    pub const SYNC_STATE_EXCHANGE: u8 = 150;
    /// `syncExport = 201` — a zipped backup export.
    pub const SYNC_EXPORT: u8 = 201;
    /// `syncSubscriptions = 202` — [`super::SyncSubscriptionsPackage`] JSON.
    pub const SYNC_SUBSCRIPTIONS: u8 = 202;
    /// `syncHistory = 203` — `List<HistoryVideo>` JSON.
    pub const SYNC_HISTORY: u8 = 203;
    /// `syncSubscriptionGroups = 204` — `SyncSubscriptionGroupsPackage` JSON.
    pub const SYNC_SUBSCRIPTION_GROUPS: u8 = 204;
    /// `syncPlaylists = 205` — `SyncPlaylistsPackage` JSON.
    pub const SYNC_PLAYLISTS: u8 = 205;
    /// `syncWatchLater = 206` — `SyncWatchLaterPackage` JSON.
    pub const SYNC_WATCH_LATER: u8 = 206;
}

/// Content-encoding of a DATA payload. Source: `ContentEncoding.kt`.
pub mod content_encoding {
    pub const RAW: u8 = 0;
    pub const GZIP: u8 = 1;
}

// ---------------------------------------------------------------------------
// Plaintext frame codec (the bytes that go *inside* the Noise cipher)
// ---------------------------------------------------------------------------

/// A decoded transport frame (post-decryption, pre-gzip-inflate).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub opcode: u8,
    pub sub_opcode: u8,
    pub content_encoding: u8,
    pub payload: Vec<u8>,
}

/// Error decoding a transport frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer than [`HEADER_SIZE`] bytes.
    TooShort,
    /// The self-describing `size` field disagreed with the buffer length —
    /// Grayjay's `handleData` throws "Incomplete packet received" here.
    SizeMismatch { declared: u32, actual: usize },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooShort => write!(f, "packet shorter than header ({HEADER_SIZE} bytes)"),
            FrameError::SizeMismatch { declared, actual } => {
                write!(
                    f,
                    "incomplete packet: size field {declared} != actual {actual}"
                )
            }
        }
    }
}

impl std::error::Error for FrameError {}

/// Build the decrypted plaintext for a transport frame:
/// `[size: u32 LE][opcode: u8][subOpcode: u8][contentEncoding: u8][payload]`
/// where `size == payload.len() + 3` (i.e. everything after the size field).
///
/// Source: `SyncSocketSession.send()` writes `putInt(remaining + HEADER_SIZE -
/// 4)` then `opcode`, `subOpcode`, `contentEncoding`, then the payload.
pub fn encode_frame(opcode: u8, sub_opcode: u8, content_encoding: u8, payload: &[u8]) -> Vec<u8> {
    let size = (payload.len() + HEADER_SIZE - 4) as u32;
    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
    out.extend_from_slice(&size.to_le_bytes());
    out.push(opcode);
    out.push(sub_opcode);
    out.push(content_encoding);
    out.extend_from_slice(payload);
    out
}

/// Inverse of [`encode_frame`], with the same `size == length - 4` validation
/// Grayjay's `handleData` performs.
///
/// # Errors
///
/// Returns `Err` if `buf` is shorter than [`HEADER_SIZE`] or its declared
/// length doesn't match the actual buffer size.
pub fn decode_frame(buf: &[u8]) -> Result<Frame, FrameError> {
    let Some(header) = buf.get(0..HEADER_SIZE) else {
        return Err(FrameError::TooShort);
    };
    let Some(&[b0, b1, b2, b3, opcode, sub_opcode, content_encoding]) = header.first_chunk::<7>()
    else {
        return Err(FrameError::TooShort);
    };
    let declared = u32::from_le_bytes([b0, b1, b2, b3]);
    if declared as usize != buf.len() - 4 {
        return Err(FrameError::SizeMismatch {
            declared,
            actual: buf.len() - 4,
        });
    }
    Ok(Frame {
        opcode,
        sub_opcode,
        content_encoding,
        payload: buf.get(HEADER_SIZE..).unwrap_or_default().to_vec(),
    })
}

// ---------------------------------------------------------------------------
// JSON package models (kotlinx.serialization property names preserved)
// ---------------------------------------------------------------------------

/// `SyncSessionData` — sent as the [`sync_opcode::SYNC_STATE_EXCHANGE`] payload
/// to tell the peer how far our state is caught up, so it only sends deltas.
///
/// Source: `sync/SyncSessionData.kt`. The two timestamps use Grayjay's
/// `OffsetDateTimeSerializer` (`serializers/OffsetDateTimeSerializer.kt`), which
/// encodes an `OffsetDateTime` as **epoch seconds (a JSON number)**; `-1` (and
/// any value `< -9_999_999_999`) decodes back to "the beginning of time", i.e.
/// "send me everything".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSessionData {
    #[serde(rename = "publicKey")]
    pub public_key: String,
    #[serde(rename = "lastHistory", default = "epoch_min")]
    pub last_history: i64,
    #[serde(rename = "lastSubscription", default = "epoch_min")]
    pub last_subscription: i64,
}

fn epoch_min() -> i64 {
    -1
}

impl SyncSessionData {
    /// Ask the peer for the complete state (both timestamps at the sentinel).
    pub fn request_all(public_key: impl Into<String>) -> Self {
        Self {
            public_key: public_key.into(),
            last_history: -1,
            last_subscription: -1,
        }
    }
}

/// `SyncSubscriptionsPackage` — the subscribed-channel set plus a tombstone map
/// of removals (channel URL → epoch-seconds of removal), so a merge can respect
/// deletions. Source: `sync/models/SyncSubscriptionsPackage.kt`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncSubscriptionsPackage {
    pub subscriptions: Vec<Subscription>,
    #[serde(rename = "subscriptionRemovals", default)]
    pub subscription_removals: BTreeMap<String, i64>,
}

/// One subscription. Source: `models/Subscription.kt`. Grayjay's `Subscription`
/// has ~20 further fields (per-content fetch flags + last-seen timestamps); they
/// all carry Kotlin defaults, so kotlinx treats them as optional on decode and
/// we simply omit them (serde ignores any extras a peer sends). `channel` is the
/// only structurally-required field; `creationTime` (epoch seconds) drives the
/// "newest subscription" delta bookkeeping in `StateSync.syncSubscriptions`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub channel: SerializedChannel,
    #[serde(rename = "creationTime", default)]
    pub creation_time: i64,
}

/// `SerializedChannel`. Source:
/// `api/media/models/channels/SerializedChannel.kt` — `id, name, thumbnail,
/// banner, subscribers, description, url, links, urlAlternatives`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SerializedChannel {
    pub id: PlatformID,
    pub name: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub subscribers: i64,
    #[serde(default)]
    pub description: Option<String>,
    pub url: String,
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    #[serde(rename = "urlAlternatives", default)]
    pub url_alternatives: Vec<String>,
}

/// `PlatformID`. Source: `api/media/PlatformID.kt` — `platform, value,
/// pluginId, claimType, claimFieldType`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformID {
    pub platform: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(rename = "pluginId", default)]
    pub plugin_id: Option<String>,
    #[serde(rename = "claimType", default)]
    pub claim_type: i32,
    #[serde(rename = "claimFieldType", default = "neg_one")]
    pub claim_field_type: i32,
}

fn neg_one() -> i32 {
    -1
}

impl SerializedChannel {
    /// Build a minimal-but-valid channel record from just a channel URL — the
    /// only field Tube tracks. Field fidelity beyond `url`/`name` is lossy (see
    /// module docs); this is enough for the peer to ingest the subscription.
    pub fn from_url(url: impl Into<String>) -> Self {
        let url = url.into();
        SerializedChannel {
            id: PlatformID {
                platform: "YouTube".to_string(),
                value: Some(url.clone()),
                plugin_id: None,
                claim_type: 0,
                claim_field_type: -1,
            },
            name: url.clone(),
            thumbnail: None,
            banner: None,
            subscribers: 0,
            description: None,
            url,
            links: BTreeMap::new(),
            url_alternatives: Vec::new(),
        }
    }
}

/// `SyncDeviceInfo` — the pairing blob a device advertises (QR / manual entry /
/// mDNS TXT). Source: `sync/internal/SyncDeviceInfo.kt` — `publicKey, addresses,
/// port, pairingCode`. `publicKey` is the device's base64 Noise static key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncDeviceInfo {
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub addresses: Vec<String>,
    pub port: i32,
    #[serde(rename = "pairingCode", default)]
    pub pairing_code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grayjay_wire_constants_and_framing_match_source() {
        // Case `opcode_values_match_grayjay_source`.
        {
            // Opcode.kt
            assert_eq!(opcode::PING, 0);
            assert_eq!(opcode::PONG, 1);
            assert_eq!(opcode::NOTIFY, 2);
            assert_eq!(opcode::STREAM, 3);
            assert_eq!(opcode::DATA, 4);
            assert_eq!(opcode::REQUEST, 5);
            assert_eq!(opcode::RESPONSE, 6);
            assert_eq!(opcode::RELAY, 7);
            // GJSyncOpcodes.kt
            assert_eq!(sync_opcode::SEND_TO_DEVICES, 101);
            assert_eq!(sync_opcode::SYNC_STATE_EXCHANGE, 150);
            assert_eq!(sync_opcode::SYNC_EXPORT, 201);
            assert_eq!(sync_opcode::SYNC_SUBSCRIPTIONS, 202);
            assert_eq!(sync_opcode::SYNC_HISTORY, 203);
            assert_eq!(sync_opcode::SYNC_SUBSCRIPTION_GROUPS, 204);
            assert_eq!(sync_opcode::SYNC_PLAYLISTS, 205);
            assert_eq!(sync_opcode::SYNC_WATCH_LATER, 206);
            // ContentEncoding.kt
            assert_eq!(content_encoding::RAW, 0);
            assert_eq!(content_encoding::GZIP, 1);
        }
        // Case `framing_constants_match_source`.
        {
            assert_eq!(MAXIMUM_PACKET_SIZE, 65519);
            assert_eq!(MAXIMUM_PACKET_SIZE_ENCRYPTED, 65535);
            assert_eq!(HEADER_SIZE, 7);
            assert_eq!(NOISE_PATTERN_MAIN, "Noise_IK_25519_ChaChaPoly_BLAKE2b");
            assert_eq!(NOISE_PATTERN_PAIRING, "Noise_N_25519_ChaChaPoly_BLAKE2b");
            assert_eq!(ANDROID_LISTEN_PORT, 12315);
        }
        // Case `frame_round_trips_and_validates_size`.
        {
            let payload = b"hello grayjay";
            let bytes = encode_frame(
                opcode::DATA,
                sync_opcode::SYNC_SUBSCRIPTIONS,
                content_encoding::GZIP,
                payload,
            );
            // size field == payload + 3 (opcode+sub+ce), matching handleData's `size == len-4`.
            let declared = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            assert_eq!(declared as usize, payload.len() + 3);
            assert_eq!(declared as usize, bytes.len() - 4);

            let frame = decode_frame(&bytes).unwrap();
            assert_eq!(frame.opcode, opcode::DATA);
            assert_eq!(frame.sub_opcode, sync_opcode::SYNC_SUBSCRIPTIONS);
            assert_eq!(frame.content_encoding, content_encoding::GZIP);
            assert_eq!(frame.payload, payload);
        }
        // Case `decode_rejects_corrupt_size`.
        {
            let mut bytes = encode_frame(opcode::PING, 0, content_encoding::RAW, b"");
            bytes[0] = 0xFF; // corrupt the declared size
            assert!(matches!(
                decode_frame(&bytes),
                Err(FrameError::SizeMismatch { .. })
            ));
            assert_eq!(decode_frame(&[0u8; 3]), Err(FrameError::TooShort));
        }
    }

    #[test]
    fn grayjay_json_packages_match_source_shapes() {
        // Case `subscriptions_package_matches_grayjay_json_shape`.
        {
            // A hand-written JSON in Grayjay's exact shape must decode.
            let json = r#"{
            "subscriptions": [
                {
                    "channel": {
                        "id": {"platform": "YouTube", "value": "UC123", "claimType": 0, "claimFieldType": -1},
                        "name": "Some Channel",
                        "thumbnail": null,
                        "banner": null,
                        "subscribers": 42,
                        "description": null,
                        "url": "https://youtube.com/channel/UC123",
                        "links": {},
                        "urlAlternatives": []
                    },
                    "creationTime": 1700000000,
                    "doNotifications": true,
                    "playbackSeconds": 99
                }
            ],
            "subscriptionRemovals": {"https://youtube.com/channel/UCgone": 1699999999}
        }"#;
            let pkg: SyncSubscriptionsPackage = serde_json::from_str(json).unwrap();
            assert_eq!(pkg.subscriptions.len(), 1);
            assert_eq!(
                pkg.subscriptions[0].channel.url,
                "https://youtube.com/channel/UC123"
            );
            assert_eq!(pkg.subscriptions[0].creation_time, 1_700_000_000);
            // Extra Grayjay fields (doNotifications/playbackSeconds) are ignored.
            assert_eq!(
                pkg.subscription_removals
                    .get("https://youtube.com/channel/UCgone"),
                Some(&1_699_999_999)
            );

            // And our own serialization round-trips.
            let reencoded = serde_json::to_string(&pkg).unwrap();
            let pkg2: SyncSubscriptionsPackage = serde_json::from_str(&reencoded).unwrap();
            assert_eq!(pkg, pkg2);
        }
        // Case `device_info_blob_parses`.
        {
            let json = r#"{"publicKey":"abc==","addresses":["192.168.1.5"],"port":12315,"pairingCode":"ABCD1234"}"#;
            let info: SyncDeviceInfo = serde_json::from_str(json).unwrap();
            assert_eq!(info.public_key, "abc==");
            assert_eq!(info.port, 12315);
            assert_eq!(info.pairing_code.as_deref(), Some("ABCD1234"));
        }
    }
}
