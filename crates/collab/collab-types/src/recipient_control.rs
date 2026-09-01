//! Stable local wire contract for admitting and observing received file shares.
//!
//! The bearer URI exists only in the request variant and its `Debug`
//! implementation is redacted. Responses contain identifiers and finite state,
//! allowing sandboxed Files to use an inherited service descriptor without
//! linking the filesystem daemon.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{FilePermission, FileRecordId};

pub const RECIPIENT_CONTROL_VERSION: u16 = 1;
pub const MAX_RECIPIENT_CONTROL_FRAME_BYTES: usize = 128 * 1024;
pub const MAX_RECIPIENT_CONTROL_CONNECTIONS: usize = 32;
pub const MAX_RECIPIENT_CONTROL_REQUESTS_PER_CONNECTION: usize = 256;
pub const MAX_RECIPIENT_MOUNT_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientSubscriptionKeyV1 {
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientFailureCodeV1 {
    Offline,
    Authentication,
    Denied,
    NotFound,
    InvalidResponse,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientFailureV1 {
    pub code: RecipientFailureCodeV1,
    pub observed_at_ms: u64,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientControlRequestV1 {
    pub protocol_version: u16,
    pub operation: RecipientControlOperationV1,
}

impl std::fmt::Debug for RecipientControlRequestV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecipientControlRequestV1")
            .field("protocol_version", &self.protocol_version)
            .field("operation", &self.operation)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RecipientControlOperationV1 {
    AdmitV2 { uri: String },
    Status { key: RecipientSubscriptionKeyV1 },
    Retry { key: RecipientSubscriptionKeyV1 },
}

impl std::fmt::Debug for RecipientControlOperationV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AdmitV2 { .. } => formatter
                .debug_struct("AdmitV2")
                .field("uri", &"<redacted>")
                .finish(),
            Self::Status { key } => formatter.debug_struct("Status").field("key", key).finish(),
            Self::Retry { key } => formatter.debug_struct("Retry").field("key", key).finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientMountStatusV1 {
    pub path: String,
    pub running: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientSubscriptionStatusV1 {
    pub key: RecipientSubscriptionKeyV1,
    pub permission: FilePermission,
    pub last_success_at_ms: Option<u64>,
    pub last_error: Option<RecipientFailureV1>,
    pub mount: Option<RecipientMountStatusV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipientControlResponseV1 {
    pub protocol_version: u16,
    pub result: RecipientControlResultV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RecipientControlResultV1 {
    Admitted {
        inserted: bool,
        status: RecipientSubscriptionStatusV1,
    },
    Status(RecipientSubscriptionStatusV1),
    Error {
        code: RecipientControlErrorCode,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecipientControlErrorCode {
    UnsupportedVersion,
    InvalidRequest,
    InvalidLink,
    Collision,
    NotFound,
    LimitReached,
    FrameTooLarge,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_debug_redacts_the_only_secret_bearing_field() {
        let secret = "qos-file://v2/payload#0123456789abcdef";
        let request = RecipientControlRequestV1 {
            protocol_version: RECIPIENT_CONTROL_VERSION,
            operation: RecipientControlOperationV1::AdmitV2 { uri: secret.into() },
        };
        assert!(!format!("{request:?}").contains(secret));
        let bytes = crate::canonical_bytes(&request).unwrap();
        let decoded: RecipientControlRequestV1 =
            crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded, request);
    }
}
