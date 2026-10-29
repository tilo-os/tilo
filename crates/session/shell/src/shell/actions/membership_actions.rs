//! Workspace membership and invitations, under the canonical ids and
//! parameter names of `tilo_action_broker::collaboration_admin`. The shell
//! adds `confirm` to each (its door requires an explicit confirmation for
//! privileged, sensitive and destructive actions) and an optional
//! `recipient` to `invitation.issue`; `invitation_params_follow_the_canonical_definitions`
//! keeps the rest identical.

use super::{ActionArgs, ActionError, ActionParam, ParamType};
pub(crate) use tilo_action_broker::collaboration_admin::{
    CANCEL_ACTION, ISSUE_ACTION, LIST_ACTION, REMOVE_ACTION, SET_ROLE_ACTION,
};

/// Return a pending invitation's original token again. Shell-only: the
/// canonical surface has no resend.
pub(crate) const RESEND_ACTION: &str = "invitation.resend";

/// The invitation actions, which run through the peer-sync network.
pub(crate) const INVITATION_ACTION_IDS: &[&str] =
    &[ISSUE_ACTION, CANCEL_ACTION, LIST_ACTION, RESEND_ACTION];

/// Parameters the shell declares beyond the canonical ones.
pub(crate) const SHELL_ONLY_PARAMS: &[&str] = &["confirm", "recipient"];

/// The relay refuses an invitation that lives longer than this.
pub(crate) const MAX_INVITATION_LIFETIME_SECONDS: u64 = 30 * 24 * 60 * 60;

/// Longest display name an invitation may record.
pub(crate) const MAX_INVITEE_NAME_CHARS: usize = 80;

const CONFIRM: ActionParam = ActionParam::required(
    "confirm",
    ParamType::Boolean,
    "Confirm this workspace administration request.",
);
const ROLE: ActionParam =
    ActionParam::required("role", ParamType::Text, "viewer, commenter, or editor");
const PARTICIPANT: ActionParam = ActionParam::required(
    "participant_id",
    ParamType::Text,
    "Participant person-root identifier",
);
const INVITE_ID: ActionParam =
    ActionParam::required("invite_id", ParamType::Text, "Invitation digest");

pub(crate) const MEMBER_ROLE_SET_PARAMS: &[ActionParam] = &[PARTICIPANT, ROLE, CONFIRM];
pub(crate) const MEMBER_REMOVE_PARAMS: &[ActionParam] = &[PARTICIPANT, CONFIRM];
pub(crate) const INVITATION_ISSUE_PARAMS: &[ActionParam] = &[
    ROLE,
    ActionParam::required(
        "expires_in_seconds",
        ParamType::Integer,
        "Invitation lifetime",
    ),
    ActionParam::optional("invitee_name", ParamType::Text, "Optional display name"),
    ActionParam::optional(
        "recipient",
        ParamType::Text,
        "The invitee's 64-character person key (or `tilo-identity:` card). \
         When given, only that person's devices can redeem the invitation.",
    ),
    CONFIRM,
];
pub(crate) const INVITATION_ID_PARAMS: &[ActionParam] = &[INVITE_ID, CONFIRM];
pub(crate) const INVITATION_LIST_PARAMS: &[ActionParam] = &[CONFIRM];

/// The shell's contract for a membership or invitation action: permission,
/// risk and result exactly as the canonical definition declares, with the
/// shell door's explicit confirmation and at-most-once retry (this
/// transport carries no caller-supplied deduplication key).
pub(crate) fn collaboration_contract(id: &str) -> tilo_actions::ActionContract {
    let canonical = match id {
        ISSUE_ACTION | RESEND_ACTION => ISSUE_ACTION,
        _ => id,
    };
    let mut contract = tilo_action_broker::collaboration_admin::action_definitions()
        .into_iter()
        .find(|definition| definition.id == canonical)
        .map(|definition| definition.contract)
        .unwrap_or_default();
    contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
    contract.retry = tilo_actions::RetryPolicy::AtMostOnce;
    contract
}

/// Decode a membership or invitation action's arguments with the canonical
/// parser, so the shell accepts exactly what every other door accepts.
pub(crate) fn collaboration_operation(
    canonical_id: &str,
    args: &ActionArgs,
) -> Result<tilo_action_broker::collaboration_admin::CollaborationAdminOperation, ActionError> {
    let mut json = args.to_json();
    for shell_only in SHELL_ONLY_PARAMS {
        json.remove(*shell_only);
    }
    tilo_action_broker::collaboration_admin::parse_operation(
        canonical_id,
        Some(&serde_json::Value::Object(json).to_string()),
    )
    .map_err(ActionError::Refused)
}
