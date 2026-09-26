//! Canonical wire DTOs for the bounded local Files share-control protocol.
//!
//! This module deliberately contains no socket, clock, server, or client I/O.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Ed25519PublicKey, FilePermission, FileRecordId};

pub const SHARE_CONTROL_VERSION: u16 = 2;
pub const MAX_NAMED_SHARE_RECIPIENT_DEVICES: usize = 32;
pub const MAX_SHARE_CONTROL_FRAME_BYTES: usize = 128 * 1024;
/// Each request and response on the socket: a big-endian `u32` length, then
/// the canonical encoding. Empty frames are invalid.
pub const SHARE_CONTROL_FRAMING: wire_frame::Framing =
    wire_frame::Framing::big_endian(MAX_SHARE_CONTROL_FRAME_BYTES).reject_empty();

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareControlRequestV1 {
    pub protocol_version: u16,
    pub operation: ShareControlOperationV1,
}

/// Stable postcard-compatible variant order. New operations require a new
/// protocol version rather than insertion or reordering here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ShareControlOperationV1 {
    CreateNamed {
        record_id: FileRecordId,
        person: Ed25519PublicKey,
        /// Certified device endpoint keys from the governed V2 identity
        /// projection. The network receiver proves the human/device binding
        /// again before any envelope bytes are disclosed.
        recipient_devices: Vec<Ed25519PublicKey>,
        permission: FilePermission,
        expires_at_ms: Option<u64>,
    },
    CreateBearerV2 {
        record_id: FileRecordId,
        permission: FilePermission,
        expires_at_ms: u64,
        /// `None` is the safe one-use default; `Some(n)` requests at most n
        /// online redemptions and is validated by the server.
        max_uses: Option<u32>,
    },
    Revoke {
        grant_id: Uuid,
    },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareControlResponseV1 {
    pub protocol_version: u16,
    pub result: ShareControlResultV1,
}

impl fmt::Debug for ShareControlResponseV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShareControlResponseV1")
            .field("protocol_version", &self.protocol_version)
            .field("result", &self.result)
            .finish()
    }
}

/// Stable postcard-compatible variant order. Bearer URI Debug output remains
/// redacted because the URI fragment contains the bearer secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ShareControlResultV1 {
    NamedCreated(NamedGrantMetadataV1),
    BearerCreatedV2 { uri: String },
    Revoked { grant_id: Uuid, revoked: bool },
    Error { code: ShareControlErrorCode },
}

impl fmt::Debug for ShareControlResultV1 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NamedCreated(metadata) => f.debug_tuple("NamedCreated").field(metadata).finish(),
            Self::BearerCreatedV2 { .. } => f
                .debug_struct("BearerCreatedV2")
                .field("uri", &"<redacted>")
                .finish(),
            Self::Revoked { grant_id, revoked } => f
                .debug_struct("Revoked")
                .field("grant_id", grant_id)
                .field("revoked", revoked)
                .finish(),
            Self::Error { code } => f.debug_struct("Error").field("code", code).finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedGrantMetadataV1 {
    pub grant_id: Uuid,
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
    pub person: Ed25519PublicKey,
    pub permission: FilePermission,
    pub expires_at_ms: Option<u64>,
    pub issuer: Ed25519PublicKey,
    pub delivery: NamedShareDeliveryStateV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedShareDeliveryStateV1 {
    Pending,
    Delivered,
}

/// Stable postcard-compatible variant order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareControlErrorCode {
    UnsupportedVersion,
    InvalidRequest,
    RecordNotFound,
    LimitReached,
    FrameTooLarge,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The share-control socket framing is pinned and the revoke fixture has stable canonical bytes
    /// and round-trips.
    #[test]
    fn share_control_frames_and_fixtures_are_pinned() {
        // Phase: share_control_framing_is_pinned.
        // The share-control socket frames: a big-endian `u32` length, at most
        // 128 KiB, never empty. Files, the launcher and the filesystem daemon
        // are built separately and must agree.
        {
            assert_eq!(
                SHARE_CONTROL_FRAMING.encode(b"ab").unwrap(),
                [0, 0, 0, 2, b'a', b'b']
            );
            assert!(SHARE_CONTROL_FRAMING.encode(&[]).is_err());
            assert!(SHARE_CONTROL_FRAMING.encode(&vec![0; 128 * 1024]).is_ok());
            assert!(
                SHARE_CONTROL_FRAMING
                    .encode(&vec![0; 128 * 1024 + 1])
                    .is_err()
            );
            assert!(
                SHARE_CONTROL_FRAMING
                    .read(&mut &[0_u8, 0, 0, 0][..])
                    .is_err()
            );
        }

        // Phase: revoke_fixture_has_stable_canonical_bytes_and_round_trips.
        {
            let grant_id = Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
            let request = ShareControlRequestV1 {
                protocol_version: SHARE_CONTROL_VERSION,
                operation: ShareControlOperationV1::Revoke { grant_id },
            };
            let request_bytes = crate::canonical_bytes(&request).unwrap();
            assert_eq!(
                request_bytes,
                [
                    2, 2, 16, 0, 17, 34, 51, 68, 85, 102, 119, 136, 153, 170, 187, 204, 221, 238,
                    255,
                ]
            );
            assert_eq!(
                crate::canonical::from_canonical_bytes::<ShareControlRequestV1>(&request_bytes)
                    .unwrap(),
                request
            );

            let response = ShareControlResponseV1 {
                protocol_version: SHARE_CONTROL_VERSION,
                result: ShareControlResultV1::Revoked {
                    grant_id,
                    revoked: true,
                },
            };
            let response_bytes = crate::canonical_bytes(&response).unwrap();
            assert_eq!(
                response_bytes,
                [
                    2, 2, 16, 0, 17, 34, 51, 68, 85, 102, 119, 136, 153, 170, 187, 204, 221, 238,
                    255, 1,
                ]
            );
            assert_eq!(
                crate::canonical::from_canonical_bytes::<ShareControlResponseV1>(&response_bytes)
                    .unwrap(),
                response
            );
        }
    }
}
