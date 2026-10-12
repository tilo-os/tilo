//! A separate closed access domain. Version one never requests editing.
use super::{ClaimReply, ObservedOpenReply, RootReply, TargetReply};
use crate::retained_person::RetainedLaunchBinding;
use serde::{Deserialize, Serialize};
use std::io;
pub const PROTOCOL: &str = "tilo-open-selected-v2";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectedAccess {
    ReadOnly,
    EditInParent,
}
impl SelectedAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::EditInParent => "edit-in-parent",
        }
    }
    pub fn editable(self) -> bool {
        self == Self::EditInParent
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootRequest {
    pub schema: u32,
    pub request_id: String,
    pub components: Vec<Vec<u8>>,
    pub app_id: String,
    pub access: SelectedAccess,
}
impl RootRequest {
    pub fn valid(&self) -> bool {
        self.schema == 2
            && super::id(&self.request_id)
            && super::app(&self.app_id)
            && super::valid_components(&self.components)
            && self
                .components
                .last()
                .is_some_and(|leaf| std::str::from_utf8(leaf).is_ok())
    }
    pub fn resource_record(&self) -> String {
        format!(
            "selected-open-v2:{}:{}",
            self.access.as_str(),
            self.request_id
        )
    }
    /// Bounded navigation only. This conversion is never sent as a v1 grant request.
    pub fn navigation(&self) -> super::RootRequest {
        super::RootRequest {
            schema: 1,
            request_id: self.request_id.clone(),
            components: self.components.clone(),
            app_id: self.app_id.clone(),
            read_only: true,
        }
    }
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
impl Request {
    pub fn valid(&self) -> bool {
        self.protocol == PROTOCOL && self.request.valid() && self.navigation().valid()
    }
    /// Used solely to recheck the existing canonical action's exact app/resource.
    pub fn navigation(&self) -> super::Request {
        super::Request {
            protocol: super::PROTOCOL.into(),
            resource: self.resource.clone(),
            request: self.request.navigation(),
            selected_descriptor: self.selected_descriptor,
            parent_descriptor: self.parent_descriptor,
            proof_descriptor: self.proof_descriptor,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimSelected {
    pub schema: u32,
    pub request_id: String,
    pub source_operation: String,
    pub app_id: String,
    pub access: SelectedAccess,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSelected {
    pub schema: u32,
    pub request_id: String,
    pub binding: RetainedLaunchBinding,
    pub publisher_hex: String,
    pub access: SelectedAccess,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollSelected {
    pub schema: u32,
    pub request_id: String,
    pub access: SelectedAccess,
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
    StageSelectedConsumer(LaunchSelected),
    PollSelected(PollSelected),
    CancelSelected(PollSelected),
}
impl RootCommand {
    pub fn valid(&self) -> bool {
        match self {
            Self::OpenSelected(v) => v.valid(),
            Self::ClaimSelected(v) => {
                v.schema == 2
                    && super::id(&v.request_id)
                    && super::id(&v.source_operation)
                    && super::app(&v.app_id)
            }
            Self::LaunchSelected(v) | Self::StageSelectedConsumer(v) => {
                (!matches!(self, Self::StageSelectedConsumer(_))
                    || v.access == SelectedAccess::EditInParent)
                    && v.schema == 2
                    && super::id(&v.request_id)
                    && v.binding.validate().is_ok()
                    && super::digest(&v.publisher_hex)
                    && v.binding.resource_record
                        == format!("selected-open-v2:{}:{}", v.access.as_str(), v.request_id)
            }
            Self::PollSelected(v) | Self::CancelSelected(v) => {
                v.schema == 2 && super::id(&v.request_id)
            }
        }
    }
}
/// Staging is only custody; it is never a document grant or observed launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum StageResult {
    Staged {
        schema: u32,
        request_id: String,
        access: SelectedAccess,
        context_sha256: String,
    },
    Refused {
        schema: u32,
        request_id: String,
        access: SelectedAccess,
        context_sha256: String,
        reason: super::RootRefusal,
    },
}
impl StageResult {
    pub fn valid(&self) -> bool {
        match self {
            Self::Staged {
                schema,
                request_id,
                access,
                context_sha256,
            }
            | Self::Refused {
                schema,
                request_id,
                access,
                context_sha256,
                ..
            } => {
                *schema == 2
                    && super::id(request_id)
                    && *access == SelectedAccess::EditInParent
                    && super::digest(context_sha256)
            }
        }
    }
    pub fn matches(&self, request: &LaunchSelected) -> bool {
        match self {
            Self::Staged {
                request_id,
                access,
                context_sha256,
                ..
            }
            | Self::Refused {
                request_id,
                access,
                context_sha256,
                ..
            } => {
                self.valid()
                    && request_id == &request.request_id
                    && *access == request.access
                    && context_sha256 == &request.binding.context_sha256
            }
        }
    }
}
/// # Errors
/// Refuses malformed, oversized or mismatched staging results.
pub fn decode_stage(bytes: &[u8], request: &LaunchSelected) -> io::Result<StageResult> {
    let value: StageResult = super::decode(bytes)?;
    if !value.matches(request) {
        return Err(super::refused());
    }
    Ok(value)
}
/// # Errors
/// Refuses malformed or oversized staging results.
pub fn encode_stage(value: &StageResult) -> io::Result<Vec<u8>> {
    if !value.valid() {
        return Err(super::refused());
    }
    super::encode(value)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionReply {
    pub schema: u32,
    pub access: SelectedAccess,
    pub reply: RootReply,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimResult {
    pub schema: u32,
    pub access: SelectedAccess,
    pub reply: ClaimReply,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetResult {
    pub schema: u32,
    pub access: SelectedAccess,
    pub reply: TargetReply,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedReply {
    pub schema: u32,
    pub access: SelectedAccess,
    pub reply: ObservedOpenReply,
}
impl ObservedReply {
    pub fn valid(&self) -> bool {
        self.schema == 2 && self.reply.valid()
    }
}
fn refused() -> io::Error {
    super::refused()
}
/// # Errors
/// Refuses malformed, oversized or inconsistent version two wire values.
pub fn decode_request(bytes: &[u8]) -> io::Result<Request> {
    let v: Request = super::decode(bytes)?;
    if !v.valid() {
        return Err(refused());
    }
    Ok(v)
}
/// # Errors
/// Refuses malformed, oversized or inconsistent version two wire values.
pub fn decode_command(bytes: &[u8]) -> io::Result<RootCommand> {
    let v: RootCommand = super::decode(bytes)?;
    if !v.valid() {
        return Err(refused());
    }
    Ok(v)
}
/// # Errors
/// Refuses malformed, oversized or inconsistent version two wire values.
pub fn decode_admission(bytes: &[u8], access: SelectedAccess) -> io::Result<RootReply> {
    let v: AdmissionReply = super::decode(bytes)?;
    if v.schema != 2 || v.access != access || !v.reply.valid() {
        return Err(refused());
    }
    Ok(v.reply)
}
/// # Errors
/// Refuses malformed, oversized or inconsistent version two wire values.
pub fn decode_claim(bytes: &[u8], access: SelectedAccess) -> io::Result<ClaimReply> {
    let v: ClaimResult = super::decode(bytes)?;
    if v.schema != 2 || v.access != access || !v.reply.valid() {
        return Err(refused());
    }
    Ok(v.reply)
}
/// # Errors
/// Refuses malformed, oversized or inconsistent version two wire values.
pub fn decode_target(bytes: &[u8], access: SelectedAccess) -> io::Result<TargetReply> {
    let v: TargetResult = super::decode(bytes)?;
    if v.schema != 2 || v.access != access || !v.reply.valid() {
        return Err(refused());
    }
    Ok(v.reply)
}
/// # Errors
/// Refuses malformed, oversized or inconsistent version two wire values.
pub fn encode_observed(v: &ObservedReply) -> io::Result<Vec<u8>> {
    if !v.valid() {
        return Err(refused());
    }
    super::encode(v)
}
/// # Errors
/// Refuses malformed, oversized or inconsistent version two wire values.
pub fn decode_observed(bytes: &[u8]) -> io::Result<ObservedReply> {
    let v: ObservedReply = super::decode(bytes)?;
    if !v.valid() {
        return Err(refused());
    }
    Ok(v)
}
pub fn is_envelope(bytes: &[u8]) -> bool {
    super::decode::<serde_json::Value>(bytes)
        .is_ok_and(|v| v.get("protocol").and_then(serde_json::Value::as_str) == Some(PROTOCOL))
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    fn request(access: SelectedAccess) -> RootRequest {
        RootRequest {
            schema: 2,
            request_id: ID.into(),
            components: vec![b"report.txt".to_vec()],
            app_id: "so.tilo.text-editor".into(),
            access,
        }
    }
    #[test]
    fn editing_intent_is_a_closed_separate_wire_and_context_domain() {
        let editing = request(SelectedAccess::EditInParent);
        assert_eq!(
            editing.resource_record(),
            format!("selected-open-v2:edit-in-parent:{ID}")
        );
        assert_ne!(
            editing.resource_record(),
            request(SelectedAccess::ReadOnly).resource_record()
        );
        let bytes = super::super::encode(&RootCommand::OpenSelected(editing)).expect("v2 wire");
        assert!(decode_command(&bytes).is_ok());
        assert!(super::super::decode_command(&bytes).is_err());
        let old =
            super::super::RootCommand::OpenSelected(request(SelectedAccess::ReadOnly).navigation());
        assert!(decode_command(&super::super::encode(&old).expect("v1 wire")).is_err());
        let mut unknown = serde_json::from_slice::<serde_json::Value>(&bytes).expect("wire value");
        unknown["request"]["access"] = "administrator".into();
        assert!(decode_command(&serde_json::to_vec(&unknown).expect("invalid wire")).is_err());
    }
    #[test]
    fn access_echo_cannot_upgrade_and_invalid_utf8_cannot_alias_an_edit_path() {
        let reply = ClaimResult {
            schema: 2,
            access: SelectedAccess::ReadOnly,
            reply: ClaimReply::Claimed {
                schema: 1,
                request_id: ID.into(),
            },
        };
        let bytes = super::super::encode(&reply).expect("claim wire");
        assert!(decode_claim(&bytes, SelectedAccess::ReadOnly).is_ok());
        assert!(decode_claim(&bytes, SelectedAccess::EditInParent).is_err());
        let mut invalid = request(SelectedAccess::EditInParent);
        invalid.components = vec![vec![0xff]];
        assert!(
            decode_command(
                &super::super::encode(&RootCommand::OpenSelected(invalid)).expect("invalid wire")
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod staging_wire_tests {
    use super::*;
    fn launch() -> LaunchSelected {
        let request_id = "11111111-1111-4111-8111-111111111111".to_string();
        LaunchSelected {
            schema: 2,
            request_id: request_id.clone(),
            access: SelectedAccess::EditInParent,
            publisher_hex: "a".repeat(64),
            binding: RetainedLaunchBinding {
                operation_id: "22222222-2222-4222-8222-222222222222".into(),
                app_id: "so.tilo.text-editor".into(),
                package_sha256: "b".repeat(64),
                room: "33333333-3333-4333-8333-333333333333".into(),
                resource_record: format!("selected-open-v2:edit-in-parent:{request_id}"),
                target_uid: 1000,
                target_pid: 42,
                root_pid: 1,
                context_sha256: "c".repeat(64),
            },
        }
    }
    #[test]
    fn staged_custody_is_bound_to_exact_request_context_and_access() {
        let launch = launch();
        let result = StageResult::Staged {
            schema: 2,
            request_id: launch.request_id.clone(),
            access: launch.access,
            context_sha256: launch.binding.context_sha256.clone(),
        };
        let bytes = encode_stage(&result).expect("bounded staging wire");
        assert_eq!(
            decode_stage(&bytes, &launch).expect("exact staging reply"),
            result
        );
        assert!(
            decode_observed(&bytes).is_err(),
            "staging cannot complete an OpenWith action"
        );
        let mut changed = launch.clone();
        changed.binding.context_sha256 = "d".repeat(64);
        assert!(decode_stage(&bytes, &changed).is_err());
        changed = launch.clone();
        changed.request_id = "44444444-4444-4444-8444-444444444444".into();
        assert!(decode_stage(&bytes, &changed).is_err());
        changed = launch.clone();
        changed.access = SelectedAccess::ReadOnly;
        assert!(decode_stage(&bytes, &changed).is_err());
        let mut unknown =
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("staging value");
        unknown["writable"] = true.into();
        assert!(
            decode_stage(
                &serde_json::to_vec(&unknown).expect("unknown authority wire"),
                &launch
            )
            .is_err()
        );
    }
    #[test]
    fn readonly_stage_cannot_replace_the_existing_explicit_readonly_launch() {
        let mut launch = launch();
        launch.access = SelectedAccess::ReadOnly;
        launch.binding.resource_record =
            format!("selected-open-v2:read-only:{}", launch.request_id);
        assert!(
            decode_command(
                &super::super::encode(&RootCommand::StageSelectedConsumer(launch.clone()))
                    .expect("invalid stage wire")
            )
            .is_err()
        );
        assert!(
            decode_command(
                &super::super::encode(&RootCommand::LaunchSelected(launch))
                    .expect("existing readonly wire")
            )
            .is_ok()
        );
    }
}
