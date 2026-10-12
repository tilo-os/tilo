//! Canonical broker surface for host-owned workspace collaboration administration.

use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin};
use tilo_user_identity::DeviceCert;

pub const APP_ID: &str = "tilo.workspace.collaboration";
pub const ISSUE_ACTION: &str = "invitation.issue";
pub const CANCEL_ACTION: &str = "invitation.cancel";
pub const LIST_ACTION: &str = "invitation.list";
pub const SET_ROLE_ACTION: &str = "member.role.set";
pub const REMOVE_ACTION: &str = "member.remove";
/// Operator-only: the loopback operator makes the machine owner's own session
/// an administrator of the workspace it hosts. Not offered to room members.
pub const ADMINISTRATOR_ACTION: &str = "member.administrator.set";
pub const MEDIA_CREATE_ACTION: &str = "media.call.create";
pub const MEDIA_END_ACTION: &str = "media.call.end";
pub const MEDIA_RESTART_ACTION: &str = "media.call.restart";
pub const MEDIA_DIRECTIONS_ACTION: &str = "media.call.directions.set";
pub const MEDIA_DENIAL_ACTION: &str = "media.call.participant.denial.set";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollaborationActionDefinition {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub params: Vec<tilo_actions::ActionParam>,
    pub contract: tilo_actions::ActionContract,
}

/// Canonical descriptors used by native, Cloud, and normal-OS action brokers.
pub fn action_definitions() -> Vec<CollaborationActionDefinition> {
    use tilo_actions::{
        ActionContract, ActionKind, ActionParam, ConfirmationPolicy, ParamType, ResultSpec,
        RetryPolicy, RiskLevel,
    };
    let contract = |risk| ActionContract {
        kind: ActionKind::Outside,
        risk,
        confirmation: ConfirmationPolicy::None,
        retry: RetryPolicy::RetryWithInvocationId,
        result: ResultSpec::Json {
            schema: serde_json::json!({"type":"object"}),
        },
        ..Default::default()
    };
    vec![
        CollaborationActionDefinition {
            id: ISSUE_ACTION,
            label: "Invite participant",
            description: "Issue a signed workspace invitation",
            params: vec![
                ActionParam::required("role", ParamType::Text, "viewer, commenter, or editor"),
                ActionParam::required(
                    "expires_in_seconds",
                    ParamType::Integer,
                    "Invitation lifetime",
                ),
                ActionParam::optional("invitee_name", ParamType::Text, "Optional display name"),
            ],
            contract: contract(RiskLevel::Privileged),
        },
        CollaborationActionDefinition {
            id: CANCEL_ACTION,
            label: "Revoke invitation",
            description: "Cancel an unredeemed invitation",
            params: vec![ActionParam::required(
                "invite_id",
                ParamType::Text,
                "Invitation digest",
            )],
            contract: contract(RiskLevel::Privileged),
        },
        CollaborationActionDefinition {
            id: LIST_ACTION,
            label: "List invitations",
            description: "List invitation status for the workspace owner",
            params: vec![],
            contract: contract(RiskLevel::Sensitive),
        },
        CollaborationActionDefinition {
            id: SET_ROLE_ACTION,
            label: "Change participant role",
            description: "Change a governed workspace participant role",
            params: vec![
                ActionParam::required(
                    "participant_id",
                    ParamType::Text,
                    "Participant person-root identifier",
                ),
                ActionParam::required("role", ParamType::Text, "viewer, commenter, or editor"),
            ],
            contract: contract(RiskLevel::Privileged),
        },
        CollaborationActionDefinition {
            id: REMOVE_ACTION,
            label: "Remove participant",
            description: "Remove a participant and terminate current workspace access",
            params: vec![ActionParam::required(
                "participant_id",
                ParamType::Text,
                "Participant person-root identifier",
            )],
            contract: contract(RiskLevel::Destructive),
        },
    ]
}

/// Installed only by a native owner with its operational V5 writer.
pub fn media_action_definitions() -> Vec<CollaborationActionDefinition> {
    use tilo_actions::{ActionParam as P, ParamType as T};
    // The issue action's privileged admin contract; the list is never empty.
    let contract = action_definitions()
        .into_iter()
        .next()
        .map(|definition| definition.contract)
        .unwrap_or_default();
    let counters = || {
        vec![
            P::required("call_id", T::Text, "Canonical governed call identifier"),
            P::required(
                "expected_authorization_revision",
                T::Integer,
                "Current authorization revision",
            ),
            P::required("expected_epoch", T::Integer, "Current call epoch"),
        ]
    };
    [
        (
            MEDIA_CREATE_ACTION,
            "Create call",
            vec![
                P::required("app_id", T::Text, "Exact Chat application"),
                P::required("record_id", T::Text, "Exact admitted Chat record"),
                P::required("channel_id", T::Text, "Exact voice channel"),
                P::optional(
                    "expected_authorization_revision",
                    T::Integer,
                    "Current revision; absent only before initialization",
                ),
                P::required("allowed", T::Object, "Publish and subscribe directions"),
            ],
        ),
        (MEDIA_END_ACTION, "End call", counters()),
        (MEDIA_RESTART_ACTION, "Restart call", counters()),
        (MEDIA_DIRECTIONS_ACTION, "Set call directions", {
            let mut p = counters();
            p.push(P::required(
                "allowed",
                T::Object,
                "Publish and subscribe directions",
            ));
            p
        }),
        (MEDIA_DENIAL_ACTION, "Set participant denial", {
            let mut p = counters();
            p.push(P::required(
                "person_root",
                T::Text,
                "Canonical participant person root",
            ));
            p.push(P::required(
                "denied",
                T::Object,
                "Denied publish and subscribe directions",
            ));
            p
        }),
    ]
    .into_iter()
    .map(|(id, label, params)| CollaborationActionDefinition {
        id,
        label,
        description: "Administer a durably governed workspace call",
        params,
        contract: contract.clone(),
    })
    .collect()
}

pub fn resource(room: &str) -> String {
    format!("workspace:{room}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollaborationRole {
    Viewer,
    Commenter,
    Editor,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueInvitationInput {
    pub role: CollaborationRole,
    pub expires_in_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invitee_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelInvitationInput {
    pub invite_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetMemberRoleInput {
    pub participant_id: String,
    pub role: CollaborationRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveMemberInput {
    pub participant_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaDirectionsInput {
    pub publish: bool,
    pub subscribe: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaCreateInput {
    pub app_id: String,
    pub record_id: String,
    pub channel_id: String,
    pub expected_authorization_revision: Option<u64>,
    pub allowed: MediaDirectionsInput,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaCallInput {
    pub call_id: String,
    pub expected_authorization_revision: u64,
    pub expected_epoch: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaDirectionsChangeInput {
    pub call_id: String,
    pub expected_authorization_revision: u64,
    pub expected_epoch: u64,
    pub allowed: MediaDirectionsInput,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaDenialInput {
    pub call_id: String,
    pub expected_authorization_revision: u64,
    pub expected_epoch: u64,
    pub person_root: String,
    pub denied: MediaDirectionsInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollaborationAdminOperation {
    Issue(IssueInvitationInput),
    Cancel(CancelInvitationInput),
    List,
    SetRole(SetMemberRoleInput),
    Remove(RemoveMemberInput),
    SetAdministrator(RemoveMemberInput),
    MediaCreate(MediaCreateInput),
    MediaEnd(MediaCallInput),
    MediaRestart(MediaCallInput),
    MediaSetDirections(MediaDirectionsChangeInput),
    MediaSetParticipantDenial(MediaDenialInput),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollaborationAdminRequest {
    pub room: String,
    pub invocation: tilo_actions::InvocationId,
    pub caller: DeviceCert,
    pub operation: CollaborationAdminOperation,
}

pub type CollaborationAdminFuture =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, CollaborationAdminError>> + Send>>;

pub trait CollaborationAdmin: Send + Sync {
    fn room(&self) -> &str;
    fn supports_media_governance(&self) -> bool {
        false
    }
    fn execute(&self, request: CollaborationAdminRequest) -> CollaborationAdminFuture;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollaborationAdminError {
    Rejected(String),
    Uncertain(String),
    Semantic {
        code: MediaRejectionCode,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaRejectionCode {
    Stale,
    AlreadyExists,
    InvalidState,
    Binding,
    Capacity,
    Exhausted,
}
impl MediaRejectionCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stale => "media_call_stale",
            Self::AlreadyExists => "media_call_already_exists",
            Self::InvalidState => "media_call_invalid_state",
            Self::Binding => "media_call_binding",
            Self::Capacity => "media_call_capacity",
            Self::Exhausted => "media_call_counter_exhausted",
        }
    }
}

impl std::fmt::Display for CollaborationAdminError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) | Self::Uncertain(message) | Self::Semantic { message, .. } => {
                message.fmt(formatter)
            }
        }
    }
}

impl std::error::Error for CollaborationAdminError {}

/// Decode one canonical action id and its exact JSON argument.
///
/// # Errors
///
/// Returns a message when the action is unknown or its argument is missing, oversized or malformed.
pub fn parse_operation(
    action: &str,
    argument: Option<&str>,
) -> Result<CollaborationAdminOperation, String> {
    fn parse<T: serde::de::DeserializeOwned>(argument: Option<&str>) -> Result<T, String> {
        let argument = argument.ok_or("collaboration action arguments missing")?;
        if argument.len() > 16 * 1024 {
            return Err("collaboration action arguments exceed the wire limit".into());
        }
        serde_json::from_str(argument).map_err(|error| error.to_string())
    }
    let operation = match action {
        ISSUE_ACTION => parse(argument).map(CollaborationAdminOperation::Issue),
        CANCEL_ACTION => parse(argument).map(CollaborationAdminOperation::Cancel),
        LIST_ACTION => {
            let value: serde_json::Value = parse(argument.or(Some("{}")))?;
            if value.as_object().is_none_or(|object| !object.is_empty()) {
                return Err("invitation list accepts only an empty object".into());
            }
            Ok(CollaborationAdminOperation::List)
        }
        SET_ROLE_ACTION => parse(argument).map(CollaborationAdminOperation::SetRole),
        REMOVE_ACTION => parse(argument).map(CollaborationAdminOperation::Remove),
        ADMINISTRATOR_ACTION => parse(argument).map(CollaborationAdminOperation::SetAdministrator),
        MEDIA_CREATE_ACTION => parse(argument).map(CollaborationAdminOperation::MediaCreate),
        MEDIA_END_ACTION => parse(argument).map(CollaborationAdminOperation::MediaEnd),
        MEDIA_RESTART_ACTION => parse(argument).map(CollaborationAdminOperation::MediaRestart),
        MEDIA_DIRECTIONS_ACTION => {
            parse(argument).map(CollaborationAdminOperation::MediaSetDirections)
        }
        MEDIA_DENIAL_ACTION => {
            parse(argument).map(CollaborationAdminOperation::MediaSetParticipantDenial)
        }
        _ => Err("unknown workspace collaboration action".into()),
    }?;
    let canonical = |value: &str| {
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && value.bytes().any(|b| b != b'0')
    };
    let keys: Vec<&str> = match &operation {
        CollaborationAdminOperation::MediaEnd(v) | CollaborationAdminOperation::MediaRestart(v) => {
            vec![&v.call_id]
        }
        CollaborationAdminOperation::MediaSetDirections(v) => vec![&v.call_id],
        CollaborationAdminOperation::MediaSetParticipantDenial(v) => {
            vec![&v.call_id, &v.person_root]
        }
        _ => vec![],
    };
    if keys.into_iter().any(|key| !canonical(key)) {
        return Err("media identifier must be nonzero canonical lowercase hex".into());
    }
    Ok(operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_media_actions_accept_exact_typed_arguments() {
        let id = "07".repeat(32);
        for (action, arguments) in [
            (
                "media.call.create",
                serde_json::json!({"app_id":"so.tilo.chat","record_id":"primary","channel_id":"voice","expected_authorization_revision":null,"allowed":{"publish":true,"subscribe":true}}),
            ),
            (
                "media.call.end",
                serde_json::json!({"call_id":id,"expected_authorization_revision":1,"expected_epoch":1}),
            ),
            (
                "media.call.restart",
                serde_json::json!({"call_id":id,"expected_authorization_revision":1,"expected_epoch":1}),
            ),
            (
                "media.call.directions.set",
                serde_json::json!({"call_id":id,"expected_authorization_revision":1,"expected_epoch":1,"allowed":{"publish":false,"subscribe":true}}),
            ),
            (
                "media.call.participant.denial.set",
                serde_json::json!({"call_id":id,"expected_authorization_revision":1,"expected_epoch":1,"person_root":id,"denied":{"publish":true,"subscribe":false}}),
            ),
        ] {
            assert!(
                parse_operation(action, Some(&arguments.to_string())).is_ok(),
                "{action}"
            );
        }
    }

    #[test]
    fn room_media_parser_rejects_unknown_nested_fields_and_noncanonical_keys() {
        let id = "07".repeat(32);
        let base = serde_json::json!({"call_id":id,"expected_authorization_revision":1,"expected_epoch":1,"allowed":{"publish":true,"subscribe":true}});
        for key in ["caller", "room", "epoch"] {
            let mut value = base.clone();
            value[key] = serde_json::json!("injected");
            assert!(parse_operation(MEDIA_DIRECTIONS_ACTION, Some(&value.to_string())).is_err());
        }
        let mut value = base.clone();
        value["allowed"]["owner"] = serde_json::json!(true);
        assert!(parse_operation(MEDIA_DIRECTIONS_ACTION, Some(&value.to_string())).is_err());
        for key in [
            "00".repeat(32),
            "AB".repeat(32),
            "01".repeat(31),
            format!("{id} "),
        ] {
            let mut value = base.clone();
            value["call_id"] = serde_json::json!(key);
            assert!(parse_operation(MEDIA_DIRECTIONS_ACTION, Some(&value.to_string())).is_err());
        }
        assert!(parse_operation(MEDIA_CREATE_ACTION, Some(&" ".repeat(16 * 1024 + 1))).is_err());
        assert_eq!(action_definitions().len(), 5);
        assert_eq!(
            media_action_definitions()
                .iter()
                .map(|d| d.id)
                .collect::<Vec<_>>(),
            vec![
                MEDIA_CREATE_ACTION,
                MEDIA_END_ACTION,
                MEDIA_RESTART_ACTION,
                MEDIA_DIRECTIONS_ACTION,
                MEDIA_DENIAL_ACTION
            ]
        );
        assert_eq!(MediaRejectionCode::Capacity.as_str(), "media_call_capacity");
    }

    #[test]
    fn canonical_surface_and_decoder_share_exact_action_ids() {
        let definitions = action_definitions();
        assert_eq!(
            definitions
                .iter()
                .map(|definition| definition.id)
                .collect::<Vec<_>>(),
            vec![
                ISSUE_ACTION,
                CANCEL_ACTION,
                LIST_ACTION,
                SET_ROLE_ACTION,
                REMOVE_ACTION,
            ]
        );
        assert_eq!(
            parse_operation(
                ISSUE_ACTION,
                Some(r#"{"role":"editor","expires_in_seconds":60}"#),
            )
            .unwrap(),
            CollaborationAdminOperation::Issue(IssueInvitationInput {
                role: CollaborationRole::Editor,
                expires_in_seconds: 60,
                invitee_name: None,
            })
        );
        assert!(parse_operation(LIST_ACTION, Some(r#"{"caller":"owner"}"#)).is_err());
        assert!(parse_operation("invitation.unknown", Some("{}")).is_err());
    }
}
