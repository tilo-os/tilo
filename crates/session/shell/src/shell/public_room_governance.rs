//! Public-room governance as actions: creating a public room, delegating it
//! to an always-on host, minting and revoking its standing invitation, and
//! making someone an Editor.
//!
//! One vocabulary, two doors. The shell's palette (`search::providers::
//! actions`) and the action broker — which agents, apps with a grant and
//! peers reach — offer the same actions with the same ids, parameters and
//! contract (Admin, privileged, explicit confirmation), and both turn an
//! invocation into the same [`Operation`] and then the same
//! [`CommunityRequest`] the shell's network serves. Neither door is the
//! authority: each re-checks the caller administers the room before it
//! runs, and the network refuses anything its own person may not do.

use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ConfirmationPolicy, OperationKind, ParamType,
    PermissionRequirement, RiskLevel,
};

use crate::network::invite::InviteRole;
use crate::shell::i18n::t;
use crate::shell::peer_sync_overlay::CommunityRequest;

/// Create a public room owned by this session's person.
pub const CREATE: &str = "public-room.create";
/// Record the always-on host a public room is delegated to.
pub const DELEGATE_HOST: &str = "public-room.host.delegate";
/// Mint the room's standing invitation.
pub const ISSUE_INVITE: &str = "public-room.invite.issue";
/// Stop redeeming a standing invitation.
pub const REVOKE_INVITE: &str = "public-room.invite.revoke";
/// Make a person an Editor of the room.
pub const GRANT_EDITOR: &str = "room.editor.grant";

/// The typed operation the broker's actions encode.
pub const ACTION_TYPE: &str = "tilo.public-room.governance.v1";
/// Resource kind of a room's governance surface on the broker.
pub const RESOURCE_KIND: &str = "public-room";

/// Every action id this module governs.
pub const ACTIONS: &[&str] = &[
    CREATE,
    DELEGATE_HOST,
    ISSUE_INVITE,
    REVOKE_INVITE,
    GRANT_EDITOR,
];

pub const CREATE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "name",
        ParamType::Text,
        "What the public room is called (for the community: Tilo Community).",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation: a public room is readable by anyone holding its invitation.",
    ),
];

pub const DELEGATE_HOST_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "endpoint",
        ParamType::Text,
        "The host's Iroh endpoint address as JSON, as `tilo-relay-admin endpoint` prints it.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation: every invitation this room mints will name this host.",
    ),
];

pub const ISSUE_INVITE_PARAMS: &[ActionParam] = &[
    ActionParam::optional(
        "role",
        ParamType::Text,
        "viewer or commenter (the default, and the most a standing invitation grants).",
    ),
    ActionParam::optional(
        "expires_in_days",
        ParamType::Integer,
        "End the invitation after this many days; without it, it lasts until revoked.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation: anyone holding this invitation can join the room.",
    ),
];

pub const REVOKE_INVITE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "invite_id",
        ParamType::Text,
        "The standing invitation's 64-character id.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that no one may join with this invitation any more.",
    ),
];

pub const GRANT_EDITOR_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "person",
        ParamType::Text,
        "The person's id (their 64-character user key).",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that this person may edit and triage in the room.",
    ),
];

/// One validated governance operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    Create {
        name: String,
    },
    DelegateHost {
        endpoint: String,
    },
    IssueInvite {
        role: InviteRole,
        expires_in_seconds: Option<u64>,
    },
    RevokeInvite {
        invite_id: String,
    },
    GrantEditor {
        person: String,
    },
}

/// The contract every governance action carries.
#[must_use]
pub fn contract() -> ActionContract {
    ActionContract {
        permission: PermissionRequirement::Admin,
        risk: RiskLevel::Privileged,
        confirmation: ConfirmationPolicy::Confirm,
        tags: vec!["room".into(), "public".into(), "governance".into()],
        ..ActionContract::default()
    }
}

fn is_key(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Validate `args` as the arguments of `action`: confirmation first, then
/// each parameter.
///
/// # Errors
///
/// An unconfirmed call, an unknown action or an invalid argument.
pub fn operation(action: &str, args: &ActionArgs) -> Result<Operation, String> {
    if args.boolean("confirm") != Some(true) {
        return Err("explicit confirmation is required".into());
    }
    let text = |name: &str| args.text(name).unwrap_or_default().trim().to_string();
    match action {
        CREATE => {
            let name = text("name");
            if name.is_empty() {
                return Err("a public room needs a name".into());
            }
            Ok(Operation::Create { name })
        }
        DELEGATE_HOST => {
            let endpoint = text("endpoint");
            serde_json::from_str::<iroh::EndpointAddr>(&endpoint)
                .map_err(|error| format!("endpoint is not an Iroh endpoint address: {error}"))?;
            Ok(Operation::DelegateHost { endpoint })
        }
        ISSUE_INVITE => {
            let role = match args.text("role").map_or("commenter", str::trim) {
                "" | "commenter" => InviteRole::Commenter,
                "viewer" => InviteRole::Viewer,
                other => {
                    return Err(format!(
                        "a standing invitation is viewer or commenter, not {other:?}"
                    ));
                }
            };
            let expires_in_seconds = match args.integer("expires_in_days") {
                None => None,
                Some(days) => match u64::try_from(days) {
                    Ok(days) if days >= 1 => Some(days.saturating_mul(86_400)),
                    _ => return Err("expires_in_days must be at least one day".into()),
                },
            };
            Ok(Operation::IssueInvite {
                role,
                expires_in_seconds,
            })
        }
        REVOKE_INVITE => {
            let invite_id = text("invite_id").to_ascii_lowercase();
            if !is_key(&invite_id) {
                return Err("invite_id is the invitation's 64-character id".into());
            }
            Ok(Operation::RevokeInvite { invite_id })
        }
        GRANT_EDITOR => {
            let person = text("person").to_ascii_lowercase();
            if !is_key(&person) {
                return Err("person is the person's 64-character id".into());
            }
            Ok(Operation::GrantEditor { person })
        }
        other => Err(format!("{other} is not a public-room governance action")),
    }
}

impl Operation {
    /// Whether the operation acts on an existing public room (every one but
    /// creating one).
    #[must_use]
    pub fn needs_public_room(&self) -> bool {
        !matches!(self, Self::Create { .. })
    }

    /// The network request that performs it in `room`, and the answer field
    /// worth handing back (the new room's id, a minted invitation).
    #[must_use]
    pub fn request(self, room: &str) -> (CommunityRequest, Option<&'static str>) {
        let room = room.to_string();
        match self {
            Self::Create { name } => (CommunityRequest::CreatePublicRoom { name }, Some("room")),
            Self::DelegateHost { endpoint } => {
                (CommunityRequest::DelegateHost { room, endpoint }, None)
            }
            Self::IssueInvite {
                role,
                expires_in_seconds,
            } => (
                CommunityRequest::IssueStandingInvite {
                    room,
                    role,
                    expires_in_seconds,
                },
                Some("invite"),
            ),
            Self::RevokeInvite { invite_id } => (
                CommunityRequest::RevokeStandingInvite { room, invite_id },
                None,
            ),
            Self::GrantEditor { person } => (
                CommunityRequest::GrantRole {
                    room,
                    person,
                    role: InviteRole::Editor,
                },
                None,
            ),
        }
    }
}

/// The palette's row label and subtitle keys of `action`.
#[must_use]
pub fn copy_keys(action: &str) -> (&'static str, &'static str) {
    match action {
        CREATE => (
            "search-action-public-room-create",
            "search-action-public-room-create-subtitle",
        ),
        DELEGATE_HOST => (
            "search-action-public-room-host-delegate",
            "search-action-public-room-host-delegate-subtitle",
        ),
        ISSUE_INVITE => (
            "search-action-public-room-invite-issue",
            "search-action-public-room-invite-issue-subtitle",
        ),
        REVOKE_INVITE => (
            "search-action-public-room-invite-revoke",
            "search-action-public-room-invite-revoke-subtitle",
        ),
        _ => (
            "search-action-room-editor-grant",
            "search-action-room-editor-grant-subtitle",
        ),
    }
}

fn params(action: &str) -> &'static [ActionParam] {
    match action {
        CREATE => CREATE_PARAMS,
        DELEGATE_HOST => DELEGATE_HOST_PARAMS,
        ISSUE_INVITE => ISSUE_INVITE_PARAMS,
        REVOKE_INVITE => REVOKE_INVITE_PARAMS,
        _ => GRANT_EDITOR_PARAMS,
    }
}

/// The broker resource id of `room`'s governance surface.
#[must_use]
pub fn resource_id(room: &str) -> String {
    format!("{RESOURCE_KIND}:{room}")
}

/// `workspace`'s governance surface for the broker: creating a public room
/// from any room, and — in a public room — the rest. The caller's authority
/// is decided per action by the broker, and again when it runs.
#[must_use]
pub fn resource(workspace: &crate::shell::state::Workspace) -> tilo_actions::Resource {
    let mut surface = tilo_actions::Resource::new(
        resource_id(&workspace.room.to_string()),
        RESOURCE_KIND,
        workspace.name.clone(),
    );
    let public = workspace.channel_class == collab_types::ChannelClass::Public;
    for action in ACTIONS
        .iter()
        .copied()
        .filter(|action| public || *action == CREATE)
    {
        let (label, subtitle) = copy_keys(action);
        let id = action.to_string();
        surface.actions.push(
            tilo_actions::ActionSpec::typed_operation(
                action,
                t(label),
                t(subtitle),
                OperationKind::Effect,
                ACTION_TYPE,
                params(action).to_vec(),
                move |arguments| match operation(&id, arguments) {
                    Ok(operation) => tilo_actions::EncodedAction {
                        type_id: ACTION_TYPE.into(),
                        payload: serde_json::to_vec(&operation).unwrap_or_default(),
                    },
                    Err(reason) => tilo_actions::EncodedAction {
                        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                        payload: reason.into_bytes(),
                    },
                },
            )
            .with_description(t(subtitle))
            .with_contract(contract()),
        );
    }
    surface
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(action: &str, pairs: &[(&str, serde_json::Value)]) -> ActionArgs {
        let map = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<serde_json::Map<_, _>>();
        ActionArgs::bind_json(params(action), &map).unwrap()
    }

    #[test]
    fn nothing_runs_unconfirmed_or_with_a_malformed_argument() {
        let person = "ab".repeat(32);
        assert!(
            operation(
                GRANT_EDITOR,
                &bind(
                    GRANT_EDITOR,
                    &[("person", person.clone().into()), ("confirm", false.into())]
                )
            )
            .is_err()
        );
        assert!(
            operation(
                GRANT_EDITOR,
                &bind(
                    GRANT_EDITOR,
                    &[("person", "nobody".into()), ("confirm", true.into())]
                )
            )
            .is_err()
        );
        assert!(
            operation(
                DELEGATE_HOST,
                &bind(
                    DELEGATE_HOST,
                    &[("endpoint", "{}".into()), ("confirm", true.into())]
                )
            )
            .is_err()
        );
        assert!(
            operation(
                ISSUE_INVITE,
                &bind(
                    ISSUE_INVITE,
                    &[("role", "editor".into()), ("confirm", true.into())]
                )
            )
            .is_err()
        );
        assert_eq!(
            operation(
                GRANT_EDITOR,
                &bind(
                    GRANT_EDITOR,
                    &[
                        ("person", person.to_uppercase().into()),
                        ("confirm", true.into())
                    ]
                )
            ),
            Ok(Operation::GrantEditor { person })
        );
        let endpoint = serde_json::to_string(&iroh::EndpointAddr::new(
            iroh::SecretKey::from_bytes(&[7; 32]).public(),
        ))
        .unwrap();
        assert!(matches!(
            operation(
                DELEGATE_HOST,
                &bind(
                    DELEGATE_HOST,
                    &[("endpoint", endpoint.into()), ("confirm", true.into())]
                )
            ),
            Ok(Operation::DelegateHost { .. })
        ));
    }

    #[test]
    fn the_broker_surface_offers_creation_everywhere_and_the_rest_in_public_rooms() {
        use crate::shell::state::FreshId as _;
        let mut workspace =
            crate::shell::state::Workspace::new(crate::shell::state::WorkspaceId::new(), "Team");
        let ids = |workspace: &crate::shell::state::Workspace| {
            resource(workspace)
                .actions
                .iter()
                .map(|action| action.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&workspace), vec![CREATE.to_string()]);
        workspace.channel_class = collab_types::ChannelClass::Public;
        assert_eq!(ids(&workspace).len(), ACTIONS.len());
        for action in &resource(&workspace).actions {
            assert_eq!(
                action.contract.permission,
                PermissionRequirement::Admin,
                "{}",
                action.id
            );
            assert_eq!(
                action.contract.confirmation,
                ConfirmationPolicy::Confirm,
                "{}",
                action.id
            );
        }
    }
}
