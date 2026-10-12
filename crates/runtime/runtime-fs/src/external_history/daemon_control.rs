//! Descriptor delegation on the existing authenticated helper control lane.
//! These closed records describe custody; wire fields cannot mint daemon roles.
use super::EnrollmentContext;
use serde::{Deserialize, Serialize};
use std::io;
pub const MAX_CONTROL_BYTES: usize = 64 * 1024;
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollmentRequest {
    pub schema: u32,
    pub request_id: String,
    pub person_binding: Vec<u8>,
}
impl EnrollmentRequest {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && uuid::Uuid::parse_str(&self.request_id)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == self.request_id)
            && !self.person_binding.is_empty()
            && self.person_binding.len() <= 4096
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum EnrollmentControl {
    EnrollHistoryDaemon(EnrollmentRequest),
    EnrollHistoryReadDaemon(EnrollmentRequest),
    EnrollSelectedEditorHistoryReadDaemon(EnrollmentRequest),
}
impl EnrollmentControl {
    /// # Errors
    /// Refuses invalid, mismatched or oversized control records.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let request = match self {
            Self::EnrollHistoryDaemon(request)
            | Self::EnrollHistoryReadDaemon(request)
            | Self::EnrollSelectedEditorHistoryReadDaemon(request) => request,
        };
        if !request.valid() {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > MAX_CONTROL_BYTES {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Refuses malformed, unknown, mismatched or oversized control records.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_CONTROL_BYTES {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        value.encode()?;
        Ok(value)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonDelegation {
    pub schema: u32,
    pub request_id: String,
    pub context: EnrollmentContext,
    pub daemon_pid: u32,
    pub daemon_start: u64,
    pub program_identity: [u64; 2],
}
impl DaemonDelegation {
    pub fn matches(&self, request: &EnrollmentRequest) -> bool {
        request.valid()
            && self.schema == 1
            && self.request_id == request.request_id
            && self.context.person_binding == request.person_binding
            && self.context.encode().is_ok()
            && self.daemon_pid != 0
            && self.daemon_start != 0
            && self.program_identity[1] != 0
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case", deny_unknown_fields)]
pub enum EnrollmentReply {
    Delegated { delegation: DaemonDelegation },
    Refused { schema: u32, request_id: String },
}
impl EnrollmentReply {
    /// # Errors
    /// Refuses invalid, mismatched or oversized control records.
    pub fn encode(&self, request: &EnrollmentRequest) -> io::Result<Vec<u8>> {
        let matches = match self {
            Self::Delegated { delegation } => delegation.matches(request),
            Self::Refused { schema, request_id } => {
                request.valid() && *schema == 1 && request_id == &request.request_id
            }
        };
        if !matches {
            return Err(refused());
        }
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > MAX_CONTROL_BYTES {
            return Err(refused());
        }
        Ok(bytes)
    }
    /// # Errors
    /// Refuses malformed, unknown, mismatched or oversized control records.
    pub fn decode(bytes: &[u8], request: &EnrollmentRequest) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_CONTROL_BYTES {
            return Err(refused());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        value.encode(request)?;
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_editor_read_control_is_closed_and_distinct() {
        let request = EnrollmentRequest {
            schema: 1,
            request_id: uuid::Uuid::new_v4().to_string(),
            person_binding: vec![1],
        };
        let bytes = EnrollmentControl::EnrollSelectedEditorHistoryReadDaemon(request.clone())
            .encode()
            .expect("bounded canonical control");
        let decoded = EnrollmentControl::decode(&bytes).expect("canonical decode");
        assert!(
            matches!(decoded, EnrollmentControl::EnrollSelectedEditorHistoryReadDaemon(value)
            if value.request_id == request.request_id && value.person_binding == request.person_binding)
        );
        let mut object: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        object["kind"] = serde_json::json!("enroll-selected-editor-history-read-daemon-unknown");
        assert!(EnrollmentControl::decode(&serde_json::to_vec(&object).unwrap()).is_err());
        object["kind"] = serde_json::json!("enroll-selected-editor-history-read-daemon");
        object["authority"] = serde_json::json!(true);
        assert!(EnrollmentControl::decode(&serde_json::to_vec(&object).unwrap()).is_err());
    }

    #[test]
    fn control_refuses_nil_unknown_and_oversized_binding() {
        let request = EnrollmentRequest {
            schema: 1,
            request_id: "12345678-1234-1234-1234-123456789abc".into(),
            person_binding: vec![1],
        };
        let value = EnrollmentControl::EnrollHistoryDaemon(request.clone());
        let bytes = value.encode().unwrap();
        assert!(EnrollmentControl::decode(&bytes).is_ok());
        let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json["request"]["authority"] = true.into();
        assert!(EnrollmentControl::decode(&serde_json::to_vec(&json).unwrap()).is_err());
        let mut invalid = request;
        invalid.person_binding.resize(4097, 1);
        assert!(
            EnrollmentControl::EnrollHistoryDaemon(invalid.clone())
                .encode()
                .is_err()
        );
        invalid.request_id = uuid::Uuid::nil().to_string();
        assert!(!invalid.valid());
    }
    #[test]
    fn refusal_is_only_for_the_exact_request() {
        let request = EnrollmentRequest {
            schema: 1,
            request_id: "12345678-1234-1234-1234-123456789abc".into(),
            person_binding: vec![1],
        };
        let reply = EnrollmentReply::Refused {
            schema: 1,
            request_id: request.request_id.clone(),
        };
        let bytes = reply.encode(&request).unwrap();
        assert!(EnrollmentReply::decode(&bytes, &request).is_ok());
        let mut other = request;
        other.request_id = "12345678-1234-1234-1234-123456789abd".into();
        assert!(EnrollmentReply::decode(&bytes, &other).is_err());
    }
    #[test]
    fn delegated_context_must_match_exact_request_binding() {
        let request = EnrollmentRequest {
            schema: 1,
            request_id: "12345678-1234-1234-1234-123456789abc".into(),
            person_binding: b"binding".to_vec(),
        };
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .to_bytes();
        let context = EnrollmentContext {
            schema: 1,
            nonce: [1; 32],
            person_binding: request.person_binding.clone(),
            verifier: super::super::VerifierClaims {
                schema: 1,
                source_owner: [2; 32],
                source_operation: request.request_id.clone(),
                person: key,
                personal_room: "12345678-1234-1234-1234-123456789abd".into(),
                device: key,
            },
        };
        let reply = EnrollmentReply::Delegated {
            delegation: DaemonDelegation {
                schema: 1,
                request_id: request.request_id.clone(),
                context,
                daemon_pid: 42,
                daemon_start: 43,
                program_identity: [44, 45],
            },
        };
        let bytes = reply.encode(&request).unwrap();
        assert!(EnrollmentReply::decode(&bytes, &request).is_ok());
        let mut other = request.clone();
        other.person_binding.push(0);
        assert!(EnrollmentReply::decode(&bytes, &other).is_err());
        let mut changed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        changed["delegation"]["daemon_start"] = 0.into();
        assert!(EnrollmentReply::decode(&serde_json::to_vec(&changed).unwrap(), &request).is_err());
    }
}
