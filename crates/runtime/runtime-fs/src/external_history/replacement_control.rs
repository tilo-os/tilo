//! Closed status on the existing enrolled native-history lane. Pending is
//! custody only; signed wire data never substitutes for actual source pin ACK.
use super::{CommittedReceipt, Deserialize, PreparedReceipt, Serialize, id, io, refused};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ReplacementReply {
    /// Cleanup only: exact prior RPC custody has stopped. This is not a save result.
    Retired {
        request_id: String,
        operation_id: String,
        transaction_digest: [u8; 32],
    },
    Pending {
        request_id: String,
        operation_id: String,
    },
    Refused {
        request_id: String,
        operation_id: Option<String>,
    },
    Committed {
        request_id: String,
        prepared: Box<PreparedReceipt>,
        receipt: Box<CommittedReceipt>,
    },
}
impl ReplacementReply {
    pub const LIMIT: usize = 16 * 1024;
    pub fn valid(&self) -> bool {
        match self {
            Self::Retired {
                request_id,
                operation_id,
                transaction_digest,
            } => id(request_id) && id(operation_id) && *transaction_digest != [0; 32],
            Self::Pending {
                request_id,
                operation_id,
            } => id(request_id) && id(operation_id),
            Self::Refused {
                request_id,
                operation_id,
            } => id(request_id) && operation_id.as_ref().is_none_or(|v| id(v)),
            Self::Committed {
                request_id,
                prepared,
                receipt,
            } => {
                id(request_id)
                    && prepared.signing_bytes().is_ok()
                    && prepared.signature.len() == 64
                    && receipt.signing_bytes().is_ok()
                    && receipt.signature.len() == 64
                    && prepared.operation_id == receipt.operation_id
                    && prepared.source_owner == receipt.source_owner
                    && prepared.intent_digest == receipt.intent_digest
                    && prepared.transaction_digest == receipt.transaction_digest
                    && prepared.archive_binding_digest == receipt.archive_binding_digest
                    && prepared.person == receipt.person
                    && prepared.personal_room == receipt.personal_room
                    && prepared.device == receipt.device
            }
        }
    }
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if !self.valid() {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > Self::LIMIT {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// Decode a cleanup ACK bound to the actual previously committed operation.
    /// Its wire contents never constitute Save or Read authority.
    /// # Errors
    /// Refuses malformed replies or cleanup acknowledgments for another operation.
    pub fn decode_retired(
        bytes: &[u8],
        expected_request: &str,
        expected_operation: &str,
        expected_transaction: [u8; 32],
    ) -> io::Result<Self> {
        let value = Self::decode(bytes, expected_request)?;
        if matches!(&value, Self::Retired { operation_id, transaction_digest, .. }
            if operation_id == expected_operation && *transaction_digest == expected_transaction)
        {
            Ok(value)
        } else {
            Err(refused())
        }
    }
    /// # Errors
    /// Refuses malformed, oversized, invalid, or mismatched replies.
    pub fn decode(bytes: &[u8], expected_request: &str) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > Self::LIMIT || !id(expected_request) {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        let request = match &value {
            Self::Retired { request_id, .. }
            | Self::Pending { request_id, .. }
            | Self::Refused { request_id, .. }
            | Self::Committed { request_id, .. } => request_id,
        };
        if !value.valid() || request != expected_request {
            return Err(refused());
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_history::EnrollmentCommand;
    #[test]
    fn retired_decoder_is_exact_cleanup_and_never_a_save_reply() {
        let request = "11111111-1111-4111-8111-111111111111";
        let operation = "22222222-2222-4222-8222-222222222222";
        let digest = [3; 32];
        let retired = ReplacementReply::Retired {
            request_id: request.into(),
            operation_id: operation.into(),
            transaction_digest: digest,
        }
        .encode()
        .unwrap();
        assert!(ReplacementReply::decode_retired(&retired, request, operation, digest).is_ok());
        assert!(ReplacementReply::decode_retired(&retired, request, operation, [4; 32]).is_err());
        assert!(ReplacementReply::decode_retired(&retired, operation, operation, digest).is_err());
        let pending = ReplacementReply::Pending {
            request_id: request.into(),
            operation_id: operation.into(),
        }
        .encode()
        .unwrap();
        assert!(ReplacementReply::decode_retired(&pending, request, operation, digest).is_err());
        let valid = serde_json::to_vec(&EnrollmentCommand::RetireReplacement {
            request_id: request.into(),
            successor_request_id: operation.into(),
            operation_id: operation.into(),
            transaction_digest: digest,
        })
        .unwrap();
        assert!(EnrollmentCommand::decode(&valid).is_ok());
        let same = serde_json::to_vec(&EnrollmentCommand::RetireReplacement {
            request_id: request.into(),
            successor_request_id: request.into(),
            operation_id: operation.into(),
            transaction_digest: digest,
        })
        .unwrap();
        assert!(EnrollmentCommand::decode(&same).is_err());
    }
    #[test]
    fn actual_control_decoder_refuses_wrong_request_and_unknown_success_vocabulary() {
        let request = "11111111-1111-4111-8111-111111111111";
        let other = "22222222-2222-4222-8222-222222222222";
        let pending = ReplacementReply::Pending {
            request_id: request.into(),
            operation_id: other.into(),
        };
        let bytes = pending.encode().unwrap();
        assert!(matches!(
            ReplacementReply::decode(&bytes, request).unwrap(),
            ReplacementReply::Pending { .. }
        ));
        assert!(ReplacementReply::decode(&bytes, other).is_err());
        let receipt_only=br#"{"result":"committed","request_id":"11111111-1111-4111-8111-111111111111","admitted":true}"#;
        assert!(ReplacementReply::decode(receipt_only, request).is_err());
        assert!(
            ReplacementReply::decode(&vec![b' '; ReplacementReply::LIMIT + 1], request).is_err()
        );
        assert!(
            EnrollmentCommand::decode(br#"{"method":"start-replacement","request_id":"../file"}"#)
                .is_err()
        );
        assert!(EnrollmentCommand::decode(br#"{"method":"start-replacement","request_id":"11111111-1111-4111-8111-111111111111","path":"/tmp"}"#).is_err());
    }
}
