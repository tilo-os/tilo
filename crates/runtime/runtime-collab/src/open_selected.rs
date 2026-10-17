//! Bounded navigation requests, never serialized source or edit authority.
pub mod discovery;
use serde::{Deserialize, Serialize};
use std::io;
pub const PROTOCOL: &str = "tilo-open-selected-v1";
pub const MAX_WIRE_BYTES: usize = 16 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootRequest {
    pub schema: u32,
    pub request_id: String,
    pub components: Vec<Vec<u8>>,
    pub app_id: String,
    pub read_only: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: String,
    pub resource: String,
    pub request: RootRequest,
    pub selected_descriptor: u32,
    pub parent_descriptor: u32,
    pub proof_descriptor: u32,
}
/// A root-held admission routing receipt. It grants no bytes, edit authority,
/// or assurance that a destination application has launched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootReceipt {
    pub schema: u32,
    pub request_id: String,
    pub source_operation: String,
}
fn id(value: &str) -> bool {
    value.len() == 36
        && value != "00000000-0000-0000-0000-000000000000"
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
fn valid_components(components: &[Vec<u8>]) -> bool {
    !components.is_empty()
        && components.len() <= 64
        && components.iter().all(|part| {
            !part.is_empty()
                && part.len() <= 255
                && part != b"."
                && part != b".."
                && !part.contains(&0)
                && !part.contains(&b'/')
        })
}
impl RootRequest {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && id(&self.request_id)
            && self.read_only
            && !self.app_id.is_empty()
            && self.app_id.len() <= 255
            && self.app_id.split('.').count() >= 2
            && self.app_id.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            && valid_components(&self.components)
    }
}
impl Request {
    pub fn valid(&self) -> bool {
        self.protocol == PROTOCOL
            && !self.resource.is_empty()
            && self.resource.len() <= 4096
            && !self.resource.contains('\0')
            && self.request.valid()
            && [
                self.selected_descriptor,
                self.parent_descriptor,
                self.proof_descriptor,
            ]
            .iter()
            .all(|fd| i32::try_from(*fd).is_ok())
            && self.selected_descriptor != self.parent_descriptor
            && self.selected_descriptor != self.proof_descriptor
            && self.parent_descriptor != self.proof_descriptor
    }
}
impl RootReceipt {
    pub fn valid(&self) -> bool {
        self.schema == 1 && id(&self.request_id) && id(&self.source_operation)
    }
}
fn refused() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid selected-open envelope")
}
/// # Errors
/// Refuses malformed, invalid or oversized selected-open wire values.
pub fn decode_root_request(bytes: &[u8]) -> io::Result<RootRequest> {
    let value: RootRequest = decode(bytes)?;
    if !value.valid() {
        return Err(refused());
    }
    Ok(value)
}
/// # Errors
/// Refuses malformed, invalid or oversized selected-open wire values.
pub fn encode_receipt(value: &RootReceipt) -> io::Result<Vec<u8>> {
    if !value.valid() {
        return Err(refused());
    }
    encode(value)
}
/// # Errors
/// Refuses malformed, invalid or oversized selected-open wire values.
pub fn decode_receipt(bytes: &[u8]) -> io::Result<RootReceipt> {
    let value: RootReceipt = decode(bytes)?;
    if !value.valid() {
        return Err(refused());
    }
    Ok(value)
}
/// # Errors
/// Refuses malformed, invalid or oversized selected-open wire values.
pub fn decode_request(bytes: &[u8]) -> io::Result<Request> {
    let value: Request = decode(bytes)?;
    if !value.valid() {
        return Err(refused());
    }
    Ok(value)
}
/// # Errors
/// Refuses malformed, invalid or oversized selected-open wire values.
pub fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.is_empty() || bytes.len() > MAX_WIRE_BYTES {
        return Err(refused());
    }
    Ok(bytes)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    if bytes.is_empty() || bytes.len() > MAX_WIRE_BYTES {
        return Err(refused());
    }
    serde_json::from_slice(bytes).map_err(io::Error::other)
}
/// Demultiplex only this explicit protocol; malformed protocol envelopes
/// never fall through into clipboard command parsing.
pub fn is_envelope(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_WIRE_BYTES
        && serde_json::from_slice::<serde_json::Value>(bytes)
            .is_ok_and(|v| v.get("protocol").and_then(serde_json::Value::as_str) == Some(PROTOCOL))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_refuses_traversal_write_and_unknown_authority() {
        let request = RootRequest {
            schema: 1,
            request_id: "11111111-1111-4111-8111-111111111111".into(),
            components: vec![b"file.txt".to_vec()],
            app_id: "so.tilo.text-editor".into(),
            read_only: true,
        };
        let bytes = encode(&request).expect("bounded navigation");
        assert_eq!(decode_root_request(&bytes).expect("navigation"), request);
        for component in [
            b"..".to_vec(),
            b"a/b".to_vec(),
            vec![0],
            Vec::new(),
            vec![b'a'; 256],
        ] {
            let mut bad = request.clone();
            bad.components = vec![component];
            assert!(
                decode_root_request(&encode(&bad).expect("encode invalid navigation")).is_err()
            );
        }
        let mut bad = request.clone();
        bad.read_only = false;
        assert!(decode_root_request(&encode(&bad).expect("encode invalid mode")).is_err());
        let mut claimed = serde_json::to_value(&request).expect("wire");
        claimed["uid"] = 0.into();
        assert!(decode_root_request(&serde_json::to_vec(&claimed).expect("wire")).is_err());
        assert!(decode_root_request(&vec![b' '; MAX_WIRE_BYTES + 1]).is_err());
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RootRefusal {
    Unavailable,
    ResidentLimit,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum RootReply {
    Admitted {
        receipt: RootReceipt,
    },
    Refused {
        schema: u32,
        request_id: String,
        reason: RootRefusal,
    },
}
impl RootReply {
    pub fn valid(&self) -> bool {
        match self {
            Self::Admitted { receipt } => receipt.valid(),
            Self::Refused {
                schema, request_id, ..
            } => *schema == 1 && id(request_id),
        }
    }
    pub fn request_id(&self) -> &str {
        match self {
            Self::Admitted { receipt } => &receipt.request_id,
            Self::Refused { request_id, .. } => request_id,
        }
    }
}
/// # Errors
/// Refuses malformed, invalid or oversized selected-open wire values.
pub fn encode_reply(reply: &RootReply) -> io::Result<Vec<u8>> {
    if !reply.valid() {
        return Err(refused());
    }
    encode(reply)
}
/// # Errors
/// Refuses malformed, invalid or oversized selected-open wire values.
pub fn decode_reply(bytes: &[u8]) -> io::Result<RootReply> {
    let value: RootReply = decode(bytes)?;
    if !value.valid() {
        return Err(refused());
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimSelected {
    pub schema: u32,
    pub request_id: String,
    pub source_operation: String,
    pub app_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSelected {
    pub schema: u32,
    pub request_id: String,
    pub binding: crate::retained_person::RetainedLaunchBinding,
    pub publisher_hex: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollSelected {
    pub schema: u32,
    pub request_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum RootCommand {
    OpenSelected(RootRequest),
    ClaimSelected(ClaimSelected),
    LaunchSelected(LaunchSelected),
    PollSelected(PollSelected),
    CancelSelected(PollSelected),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClaimReply {
    Claimed {
        schema: u32,
        request_id: String,
    },
    Refused {
        schema: u32,
        request_id: String,
        reason: RootRefusal,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetNamespaceReceipt {
    pub schema: u32,
    pub request_id: String,
    pub app_id: String,
    pub package_sha256: String,
    pub launcher_pid: u32,
    pub launcher_start: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetReply {
    Pending {
        schema: u32,
        request_id: String,
    },
    NamespaceReady {
        receipt: TargetNamespaceReceipt,
    },
    Refused {
        schema: u32,
        request_id: String,
        reason: RootRefusal,
    },
    CancelAccepted {
        schema: u32,
        request_id: String,
    },
}
/// An observation of both the real target namespace and its adopted window.
/// Only the ordinary Shell launch observer produces this RPC response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedOpenReply {
    pub schema: u32,
    pub request_id: String,
    pub source_operation: String,
    pub target: TargetNamespaceReceipt,
}
fn app(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('.').count() >= 2
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl ClaimSelected {
    pub fn valid(&self) -> bool {
        self.schema == 1 && id(&self.request_id) && id(&self.source_operation) && app(&self.app_id)
    }
}
impl LaunchSelected {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && id(&self.request_id)
            && self.binding.validate().is_ok()
            && self.binding.resource_record == format!("selected-open:{}", self.request_id)
            && digest(&self.publisher_hex)
    }
}
impl PollSelected {
    pub fn valid(&self) -> bool {
        self.schema == 1 && id(&self.request_id)
    }
}
impl RootCommand {
    pub fn valid(&self) -> bool {
        match self {
            Self::OpenSelected(v) => v.valid(),
            Self::ClaimSelected(v) => v.valid(),
            Self::LaunchSelected(v) => v.valid(),
            Self::PollSelected(v) | Self::CancelSelected(v) => v.valid(),
        }
    }
}
impl ClaimReply {
    pub fn valid(&self) -> bool {
        match self {
            Self::Claimed { schema, request_id }
            | Self::Refused {
                schema, request_id, ..
            } => *schema == 1 && id(request_id),
        }
    }
    pub fn request_id(&self) -> &str {
        match self {
            Self::Claimed { request_id, .. } | Self::Refused { request_id, .. } => request_id,
        }
    }
}
impl TargetNamespaceReceipt {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && id(&self.request_id)
            && app(&self.app_id)
            && digest(&self.package_sha256)
            && self.launcher_pid > 1
            && self.launcher_start > 0
    }
}
impl TargetReply {
    pub fn valid(&self) -> bool {
        match self {
            Self::NamespaceReady { receipt } => receipt.valid(),
            Self::Pending { schema, request_id }
            | Self::Refused {
                schema, request_id, ..
            }
            | Self::CancelAccepted { schema, request_id } => *schema == 1 && id(request_id),
        }
    }
    pub fn request_id(&self) -> &str {
        match self {
            Self::NamespaceReady { receipt } => &receipt.request_id,
            Self::Pending { request_id, .. }
            | Self::Refused { request_id, .. }
            | Self::CancelAccepted { request_id, .. } => request_id,
        }
    }
}
impl ObservedOpenReply {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && id(&self.request_id)
            && id(&self.source_operation)
            && self.target.valid()
            && self.target.request_id == self.request_id
    }
}
/// # Errors
/// Refuses unknown commands, malformed labels or values exceeding the bound.
pub fn decode_command(bytes: &[u8]) -> io::Result<RootCommand> {
    let value: RootCommand = decode(bytes)?;
    if !value.valid() {
        return Err(refused());
    }
    Ok(value)
}
/// # Errors
/// Refuses malformed or oversized claim replies.
pub fn decode_claim_reply(bytes: &[u8]) -> io::Result<ClaimReply> {
    let value: ClaimReply = decode(bytes)?;
    if !value.valid() {
        return Err(refused());
    }
    Ok(value)
}
/// # Errors
/// Refuses malformed or oversized target replies.
pub fn decode_target_reply(bytes: &[u8]) -> io::Result<TargetReply> {
    let value: TargetReply = decode(bytes)?;
    if !value.valid() {
        return Err(refused());
    }
    Ok(value)
}
/// # Errors
/// Refuses malformed or oversized observed launch replies.
pub fn encode_observed(value: &ObservedOpenReply) -> io::Result<Vec<u8>> {
    if !value.valid() {
        return Err(refused());
    }
    encode(value)
}
/// # Errors
/// Refuses malformed or oversized observed launch replies.
pub fn decode_observed(bytes: &[u8]) -> io::Result<ObservedOpenReply> {
    let value: ObservedOpenReply = decode(bytes)?;
    if !value.valid() {
        return Err(refused());
    }
    Ok(value)
}
/// Destination spelling derived from an already bounded navigation component.
/// It grants no access to the original path or source filesystem.
/// # Errors
/// Refuses an invalid component sequence.
pub fn selected_destination(components: &[Vec<u8>]) -> io::Result<String> {
    if !valid_components(components) {
        return Err(refused());
    }
    let leaf = components.last().ok_or_else(refused)?;
    Ok(format!(
        "/run/tilo/selected-open/{}",
        String::from_utf8_lossy(leaf)
    ))
}

#[cfg(test)]
mod target_wire_tests {
    use super::*;
    const REQUEST: &str = "11111111-1111-4111-8111-111111111111";
    const OPERATION: &str = "22222222-2222-4222-8222-222222222222";
    #[test]
    fn admission_and_namespace_receipts_cannot_decode_as_observed_launch() {
        let admission = RootReceipt {
            schema: 1,
            request_id: REQUEST.into(),
            source_operation: OPERATION.into(),
        };
        assert!(decode_observed(&encode_receipt(&admission).expect("admission wire")).is_err());
        let namespace = TargetNamespaceReceipt {
            schema: 1,
            request_id: REQUEST.into(),
            app_id: "so.tilo.document".into(),
            package_sha256: "a".repeat(64),
            launcher_pid: 42,
            launcher_start: 1,
        };
        assert!(
            decode_observed(
                &encode(&TargetReply::NamespaceReady {
                    receipt: namespace.clone()
                })
                .expect("namespace wire")
            )
            .is_err()
        );
        let observed = ObservedOpenReply {
            schema: 1,
            request_id: REQUEST.into(),
            source_operation: OPERATION.into(),
            target: namespace,
        };
        let bytes = encode_observed(&observed).expect("observed wire");
        assert_eq!(decode_observed(&bytes).expect("observed reply"), observed);
        let mut different = observed;
        different.target.request_id = OPERATION.into();
        assert!(decode_observed(&encode(&different).expect("changed wire")).is_err());
    }
    #[test]
    fn closed_commands_refuse_loose_navigation_and_unknown_authority_labels() {
        let request = RootRequest {
            schema: 1,
            request_id: REQUEST.into(),
            components: vec![b"report.txt".to_vec()],
            app_id: "so.tilo.document".into(),
            read_only: true,
        };
        assert!(decode_command(&encode(&request).expect("old loose wire")).is_err());
        let command = RootCommand::OpenSelected(request);
        assert_eq!(
            decode_command(&encode(&command).expect("closed command")).expect("bounded command"),
            command
        );
        let mut spoofed = serde_json::to_value(&command).expect("wire value");
        spoofed["root_pid"] = 1.into();
        assert!(decode_command(&serde_json::to_vec(&spoofed).expect("spoofed wire")).is_err());
        assert!(selected_destination(&[b"..".to_vec()]).is_err());
        assert_eq!(
            selected_destination(&[b"folder".to_vec(), b"report.txt".to_vec()])
                .expect("fixed destination"),
            "/run/tilo/selected-open/report.txt"
        );
    }
}

/// Explicit version two selected access intent; never a source grant.
pub mod v2;
