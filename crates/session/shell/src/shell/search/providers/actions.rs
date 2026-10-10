//! Shell-action provider: palette rows exposed through the provider
//! protocol, under the same chip strip + grouping the notes/files providers
//! use.
//!
//! Most rows are nullary — "toggle the overview" needs nothing but the
//! instruction. `Action::params` lets one take a typed value instead,
//! for an action whose input (the text to insert, say) isn't discoverable
//! from the chrome and has to be supplied and declared, so enumeration
//! tells a caller what to pass rather than leaving it to be inferred.

use crate::shell::state::WorkspaceAuthority as _;
use getrandom::{SysRng, rand_core::UnwrapErr};
use settings_store::display_mode::DisplayLayoutMode;
use settings_store::hardware::ShortcutAction;
use std::rc::Rc;
use tilo_clock::unix_now;
use ui::search::{FieldWeight, ListIndex};

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

mod native_identity;

/// What `record.bring` must be told.
const RECORD_BRING_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "id",
        ParamType::Text,
        "The record's id — `file:<absolute path>`, `tab:<id>`, or a minted id.",
    ),
    ActionParam::optional(
        "kind",
        ParamType::Text,
        "The record's type, in the owning app's vocabulary. Defaults to `document`.",
    ),
];

/// What `agent.invite` may be told. Naming is optional because `tilo-agent`
/// defaults to `Claude`; the profile is optional because the safe Observer
/// starting point still requires an explicit per-surface review.
const AGENT_INVITE_PARAMS: &[ActionParam] = &[
    ActionParam::optional(
        "name",
        ParamType::Text,
        "The agent's name, as it was launched with. Defaults to `Claude`.",
    ),
    ActionParam::optional(
        "preset",
        ParamType::Text,
        "Observer, Contributor, Operator, or Custom. Presets never grant access to a surface by themselves.",
    ),
];

/// The agent this OS ships. Kept in step with `tilo-agent`'s own default by
/// being the same string, not by being imported — the shell does not
/// depend on the agent binary.
const DEFAULT_AGENT_NAME: &str = "Claude";

const IDENTITY_CONNECT_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "path",
        ParamType::Text,
        "Absolute path to an exported Tilo identity JSON record.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Confirm importing credentials and delegating workspace administration.",
    ),
];

const LANGUAGE_PARAMS: &[ActionParam] = &[ActionParam::required(
    "language",
    ParamType::Text,
    "BCP 47 tag or name: en-US, de-DE, English, or German.",
)];

const IDENTITY_SELECT_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "id",
        ParamType::Text,
        "The identity's 64-character public-key fingerprint.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Confirm delegating workspace administration to this identity.",
    ),
];

const SEARCH_PROVIDER_SET_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "provider",
        ParamType::Text,
        "Stable provider id returned by search/action enumeration.",
    ),
    ActionParam::required(
        "enabled",
        ParamType::Boolean,
        "Whether this source participates in search, indexing, and agent paging.",
    ),
];

const PRESENCE_FOLLOW_PARAMS: &[ActionParam] = &[ActionParam::required(
    "collaborator",
    ParamType::Text,
    "Stable collaborator id returned by the workspace roster.",
)];

const SCREENSHOT_SEND_PARAMS: &[ActionParam] = &[ActionParam::required(
    "target",
    ParamType::Text,
    "`workspace` to save into this workspace's files, or the id of one of your online devices.",
)];

const PRESENCE_STATUS_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "text",
    ParamType::Text,
    "Manual status visible to this workspace; empty clears it.",
)];

const PRESENCE_SHARING_PARAMS: &[ActionParam] = &[ActionParam::required(
    "mode",
    ParamType::Text,
    "off, manual, or automatic.",
)];

const DEVICE_REVOKE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "device",
        ParamType::Text,
        "The enrolled device's 64-character public-key fingerprint.",
    ),
    ActionParam::optional(
        "reason",
        ParamType::Text,
        "lost, stolen, retired, or compromised (defaults to retired).",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Explicit confirmation that this permanent administrative revocation should run.",
    ),
];

fn alarm_action_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        retry: tilo_actions::RetryPolicy::Idempotent,
        tags: vec!["desktop".into(), "notification".into(), "alarm".into()],
        ..tilo_actions::ActionContract::default()
    }
}

fn is_alarm_action(id: &str) -> bool {
    crate::shell::notification::alarm_action_address(id).is_some()
        || crate::shell::notification::is_snoozed_alarm_action(id)
}

/// Contracts that come from a definition of their own rather than the
/// id table in [`shell_action_contract`].
fn dedicated_contract(id: &str) -> Option<tilo_actions::ActionContract> {
    if is_alarm_action(id) {
        return Some(alarm_action_contract());
    }
    collaboration_contract(id)
}

/// Actions that only open, inspect or dismiss shell views.
const VIEW_ACTION_IDS: &[&str] = &[
    "desktop-tour.open",
    "topbar.editor.open",
    "settings.open",
    "settings.members.open",
    "settings.privacy.open",
    "settings.apps.open",
    "quick-settings.open",
    "quick-settings.clipboard",
    "quick-settings.devices",
    "projection.open",
    "capture.open",
    "collaborators.open",
    "room.activity.open",
    "room.activity.explore",
    "room.activity.replay",
    "room.activity.acknowledge",
    "room.activity.tasks",
    "tab.menu",
    "tab.drag.cancel",
    "history.compare",
    "history.undo",
    "history.restore",
];

pub(crate) fn shell_action_contract(id: &str) -> tilo_actions::ActionContract {
    use tilo_actions::{ActionContract, ActionKind, ConfirmationPolicy, RetryPolicy, RiskLevel};

    if let Some(contract) = dedicated_contract(id) {
        return contract;
    }
    let mut contract = ActionContract::default();
    match id {
        "shortcut-help.toggle" | "overview.toggle" => {
            contract = ActionContract::view();
            contract.retry = RetryPolicy::AtMostOnce;
            contract.reversible = true;
        }
        id if VIEW_ACTION_IDS.contains(&id) => contract = ActionContract::view(),
        "identity.connect" | "identity.select" | "identity.device.revoke" => {
            contract.kind = ActionKind::Outside;
            contract.risk = RiskLevel::Privileged;
            contract.confirmation = ConfirmationPolicy::Confirm;
        }
        "identity.devices.copy"
        | "identity.device.enroll"
        | "identity.home-peer.enroll"
        | "peer.invite.named"
        | "agent.invite" => {
            contract.kind = ActionKind::Outside;
        }
        "peer.invite" | "peer.connect" => {
            contract = ActionContract::view();
            contract.kind = ActionKind::Outside;
        }
        // Moving a tab between groups is layout, undone by moving it back;
        // a close is undone by `tab.reopen`, which puts it back in place.
        "record.bring"
        | "tab.new.terminal.here"
        | "window.new.terminal"
        | "tab.duplicate"
        | "tab.open-beside"
        | "tab.split-right"
        | "tab.move-to-new-group"
        | "tab.close"
        | "tab.reopen" => {
            contract.kind = ActionKind::Undoable;
            contract.reversible = true;
        }
        "notifications.dnd.toggle"
        | "appearance.theme.dark"
        | "appearance.theme.light"
        | "appearance.high-contrast"
        | "appearance.language" => {
            contract.kind = ActionKind::Undoable;
            contract.retry = RetryPolicy::AtMostOnce;
            contract.reversible = true;
        }
        QS_BRIGHTNESS_ID | QS_VOLUME_ID | QS_WIFI_ID | QS_POWER_ID | QS_VPN_ID
        | QS_BLUETOOTH_ID | QS_AIRPLANE_ID => {
            contract = quick_settings_contract();
        }
        DISPLAY_EXTEND_ID
        | DISPLAY_MIRROR_ID
        | DISPLAY_BUILTIN_ONLY_ID
        | DISPLAY_EXTERNAL_ONLY_ID
        | DISPLAY_KEEP_ID
        | DISPLAY_REVERT_ID => contract = display_layout_contract(),
        BLUETOOTH_PAIRING_DECLINE_ID => {
            contract.kind = ActionKind::Undoable;
            contract.retry = RetryPolicy::AtMostOnce;
        }
        // Navigation and focus each advance one step when repeated.
        id if id.starts_with("navigation.axis.") || WINDOW_FOCUS_ACTION_IDS.contains(&id) => {
            contract = ActionContract::view();
            contract.retry = RetryPolicy::AtMostOnce;
            contract.reversible = true;
        }
        // Layout, undone by the opposite move.
        id if WINDOW_LAYOUT_ACTION_IDS.contains(&id) => {
            contract.kind = ActionKind::Undoable;
            contract.retry = RetryPolicy::AtMostOnce;
            contract.reversible = true;
        }
        _ => {}
    }
    contract
}

pub(crate) fn shell_action_kind(id: &str) -> tilo_actions::OperationKind {
    match id {
        "shortcut-help.toggle" | "overview.toggle" | "peer.invite" | "peer.connect" => {
            tilo_actions::OperationKind::View
        }
        id if VIEW_ACTION_IDS.contains(&id) => tilo_actions::OperationKind::View,
        "record.bring" | "peer.invite.named" | "agent.invite" => {
            tilo_actions::OperationKind::Document
        }
        _ if id.contains("#ownership.") => tilo_actions::OperationKind::Document,
        _ if WINDOW_FOCUS_ACTION_IDS.contains(&id) || id.starts_with("navigation.axis.") => {
            tilo_actions::OperationKind::View
        }
        _ => tilo_actions::OperationKind::Effect,
    }
}

pub(crate) fn current_shell_action_authorized(state: &ChromeState, id: &str) -> bool {
    let tree = state.tree();
    let Some(workspace) = tree.desktop.active_workspace() else {
        return false;
    };
    let user = state.local_user_for_room(&workspace.room);
    let authorized = shell_action_authorized_for(workspace, &workspace.actor_for_person(&user), id);
    drop(tree);
    authorized
}

pub(crate) fn shell_action_authorized_for(
    workspace: &crate::shell::state::Workspace,
    actor: &crate::shell::state::Actor,
    id: &str,
) -> bool {
    let resource = crate::shell::search::action_api::ResourceRef::new("actions", id);
    let kind = ResourceKind::Action;
    let Some((node, kind)) = crate::shell::search::visibility::required_action(&resource, &kind)
    else {
        return true;
    };
    crate::shell::search::visibility::permits(workspace, actor, &node, kind)
}

fn active_workspace_id(
    state: &ChromeState,
) -> Result<crate::shell::state::WorkspaceId, ActionError> {
    state
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.id)
        .ok_or_else(|| ActionError::Refused("no active workspace".into()))
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the read guard's last use is the tuple that ends the block it's scoped to; \
              there is nothing after it to hoist the drop past without cloning the \
              workspace out for no real gain"
)]
fn active_identity(
    state: &ChromeState,
) -> Result<
    (
        crate::shell::state::RoomId,
        crate::identity_registry::IdentityProfile,
    ),
    ActionError,
> {
    let (room, actor, may_administer) = {
        let tree = state.tree();
        let workspace = tree
            .desktop
            .active_workspace()
            .ok_or_else(|| ActionError::Refused("no active workspace".into()))?;
        let actor = state.local_user_for_room(&workspace.room);
        (
            workspace.room,
            actor.clone(),
            workspace.may_govern_at(
                &actor,
                tilo_action_policy::roles::governance::GRANT,
                tilo_clock::unix_now(),
            ),
        )
    };
    if !may_administer {
        return Err(ActionError::Refused(format!(
            "{actor} may not administer devices for this workspace's identity"
        )));
    }
    let profile = crate::identity_registry::selected_identity_profile(&room.to_string())
        .map_err(|error| ActionError::Refused(error.to_string()))?
        .ok_or_else(|| ActionError::Refused("no identity selected".into()))?;
    Ok((room, profile))
}

const MAX_OWNERSHIP_OFFER_SECONDS: u64 = 7 * 24 * 60 * 60;

fn active_ownership_signer(
    state: &ChromeState,
    expected: collab_types::Ed25519PublicKey,
) -> Result<runtime_identity::SigningIdentity, ActionError> {
    let (_, profile) = active_identity(state)?;
    if profile.user_public_key != *expected.as_bytes() {
        return Err(ActionError::Refused(
            "the selected certified identity is not the required transfer signer".into(),
        ));
    }
    runtime_identity::load_from_path(profile.record_path)
        .map_err(|error| ActionError::Refused(format!("cannot sign ownership transfer: {error}")))
}

fn apply_ownership_action(
    state: &ChromeState,
    action_id: &str,
    arguments: &ActionArgs,
) -> Result<(), ActionError> {
    match action_id {
        "ownership.propose" => apply_ownership_proposal(state, arguments),
        "ownership.accept" => apply_ownership_acceptance(state),
        "ownership.cancel" => apply_ownership_cancellation(state),
        _ => Err(ActionError::Refused("unknown ownership action".into())),
    }
}

/// Apply one of the room's governance actions, as the person at this
/// device, from the arguments `spec` takes.
fn apply_governance_action(
    state: &ChromeState,
    spec: &tilo_actions::ActionSpec,
    arguments: &ActionArgs,
) -> Result<(), ActionError> {
    let actor = state.active_local_user();
    let encoded = spec
        .build_json(&arguments.to_json())
        .map_err(|error| ActionError::Refused(format!("invalid arguments: {error}")))?;
    if encoded.type_id != tilo_desktop::workspace_settings::GOVERNANCE_ACTION_TYPE {
        return Err(ActionError::Refused(
            "the governance action encoded no operation".into(),
        ));
    }
    let operation: tilo_desktop::workspace_settings::GovernanceOperation =
        serde_json::from_slice(&encoded.payload)
            .map_err(|error| ActionError::Refused(format!("invalid operation: {error}")))?;
    let mut tree = state.tree_mut();
    let workspace_id = tree
        .desktop
        .active_workspace()
        .map(|workspace| workspace.id)
        .ok_or_else(|| ActionError::Refused("no active workspace".into()))?;
    crate::shell::workspace_governance::apply_governance_operation(
        &mut tree,
        workspace_id,
        &actor,
        operation,
    )
    .map_err(ActionError::Refused)?;
    tree.queue_save();
    drop(tree);
    Ok(())
}

fn apply_ownership_proposal(
    state: &ChromeState,
    arguments: &ActionArgs,
) -> Result<(), ActionError> {
    let now = unix_now();
    let workspace_id = active_workspace_id(state)?;
    let recipient_text = arguments.text("recipient").unwrap_or_default().trim();
    let recipient_bytes: [u8; 32] = hex::decode(recipient_text)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| {
            ActionError::Refused(
                "choose one certified administrator identity (64 hexadecimal characters)".into(),
            )
        })?;
    let expires_unix = arguments
        .integer("expires_unix")
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| ActionError::Refused("choose a valid offer expiry".into()))?;
    let latest = now.saturating_add(MAX_OWNERSHIP_OFFER_SECONDS);
    if expires_unix <= now || expires_unix > latest {
        return Err(ActionError::Refused(
            "ownership offers must expire within the next seven days".into(),
        ));
    }
    let current_owner = state
        .tree()
        .desktop
        .active_workspace()
        .and_then(|workspace| workspace.owner().map(String::as_str))
        .and_then(|owner| hex::decode(owner).ok())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .map(collab_types::Ed25519PublicKey::from_bytes)
        .ok_or_else(|| ActionError::Refused("workspace owner is not certified".into()))?;
    let signer = active_ownership_signer(state, current_owner)?;
    let mut proposal_id = [0_u8; 32];
    rand_core::Rng::fill_bytes(&mut UnwrapErr(SysRng), &mut proposal_id);
    let room = state
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.room.to_string())
        .ok_or_else(|| ActionError::Refused("no active workspace".into()))?;
    let mut proposal = collab_types::OwnershipTransferProposalV1 {
        version: collab_types::OWNERSHIP_TRANSFER_VERSION,
        workspace: room,
        proposal_id,
        current_owner,
        recipient: collab_types::Ed25519PublicKey::from_bytes(recipient_bytes),
        expires_unix,
        signature: collab_types::Signature::from_bytes([0; 64]),
    };
    proposal.signature = collab_types::Signature::from_bytes(
        signer
            .sign(&proposal.signing_bytes().map_err(|error| {
                ActionError::Refused(format!("cannot encode ownership offer: {error}"))
            })?)
            .map_err(|error| ActionError::Refused(error.to_string()))?,
    );
    state
        .tree_mut()
        .propose_workspace_ownership_transfer(workspace_id, proposal, now)
        .map_err(|error| ActionError::Refused(error.to_string()))
}

fn apply_ownership_acceptance(state: &ChromeState) -> Result<(), ActionError> {
    let now = unix_now();
    let workspace_id = active_workspace_id(state)?;
    let proposal = state
        .tree()
        .desktop
        .active_workspace()
        .and_then(|workspace| workspace.pending_ownership_transfer.clone())
        .ok_or_else(|| ActionError::Refused("no ownership offer is pending".into()))?;
    let signer = active_ownership_signer(state, proposal.recipient)?;
    let mut acceptance = collab_types::OwnershipTransferAcceptanceV1 {
        version: collab_types::OWNERSHIP_TRANSFER_VERSION,
        proposal_digest: proposal
            .digest()
            .map_err(|error| ActionError::Refused(error.to_string()))?,
        recipient: proposal.recipient,
        accepted_unix: now,
        signature: collab_types::Signature::from_bytes([0; 64]),
    };
    acceptance.signature = collab_types::Signature::from_bytes(
        signer
            .sign(&acceptance.signing_bytes().map_err(|error| {
                ActionError::Refused(format!("cannot encode ownership acceptance: {error}"))
            })?)
            .map_err(|error| ActionError::Refused(error.to_string()))?,
    );
    let audit = collab_types::OwnershipTransferAuditV1::new(proposal, acceptance, now)
        .map_err(|error| ActionError::Refused(error.to_string()))?;
    state
        .tree_mut()
        .accept_workspace_ownership_transfer(workspace_id, audit, now)
        .map_err(|error| ActionError::Refused(error.to_string()))
}

fn apply_ownership_cancellation(state: &ChromeState) -> Result<(), ActionError> {
    let now = unix_now();
    let workspace_id = active_workspace_id(state)?;
    let proposal = state
        .tree()
        .desktop
        .active_workspace()
        .and_then(|workspace| workspace.pending_ownership_transfer.clone())
        .ok_or_else(|| ActionError::Refused("no ownership offer is pending".into()))?;
    let signer = active_ownership_signer(state, proposal.current_owner)?;
    let mut cancellation = collab_types::OwnershipTransferCancellationV1 {
        version: collab_types::OWNERSHIP_TRANSFER_VERSION,
        proposal_digest: proposal
            .digest()
            .map_err(|error| ActionError::Refused(error.to_string()))?,
        current_owner: proposal.current_owner,
        cancelled_unix: now,
        signature: collab_types::Signature::from_bytes([0; 64]),
    };
    cancellation.signature = collab_types::Signature::from_bytes(
        signer
            .sign(&cancellation.signing_bytes().map_err(|error| {
                ActionError::Refused(format!("cannot encode ownership cancellation: {error}"))
            })?)
            .map_err(|error| ActionError::Refused(error.to_string()))?,
    );
    state
        .tree_mut()
        .cancel_workspace_ownership_transfer(workspace_id, &cancellation)
        .map_err(|error| ActionError::Refused(error.to_string()))
}

/// The active workspace's id, room and the caller's own actor id, refusing
/// up front if the caller may not administer it — shared by `identity.connect`
/// and `identity.select`, which otherwise change identity via identical
/// pre-checks. One lock scope reads and destructures the workspace
/// (`WorkspaceId` and `RoomId` are `Copy`; `actor` is cloned out), so the
/// tree's read guard is already gone before the caller does anything else.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the read guard's last use is the tuple that ends the block it's scoped to; \
              there is nothing after it to hoist the drop past without cloning the \
              workspace out for no real gain"
)]
/// Let the identity `principal` govern `workspace_id` as fully as `actor`:
/// a member of the room, granted everything in it.
fn delegate_room_to_identity(
    tree: &mut crate::shell::state::WorkspaceTree,
    workspace_id: crate::shell::state::WorkspaceId,
    actor: &String,
    principal: &str,
) -> Option<()> {
    use tilo_action_policy::{Answer, Change, Grant, KindSet, Role, Subject};
    let workspace = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)?;
    if workspace.is_owner(principal) {
        return Some(());
    }
    let mut changes = Vec::new();
    if workspace.role_of(principal).is_none() {
        changes.push(Change::AddMember {
            person: principal.to_owned(),
            role: Role::Viewer,
        });
    }
    changes.push(Change::Grant(Grant::new(
        format!("identity:{principal}"),
        Subject::person(principal),
        workspace.node(),
        KindSet::ALL,
        Answer::Allow,
        Subject::person(actor.clone()),
    )));
    tree.govern_in_workspace(workspace_id, actor, changes)
        .ok()
        .map(drop)
}

fn active_admin_workspace(
    state: &ChromeState,
) -> Result<
    (
        crate::shell::state::WorkspaceId,
        crate::shell::state::RoomId,
        String,
    ),
    ActionError,
> {
    let tree = state.tree();
    let Some(workspace) = tree.desktop.active_workspace() else {
        return Err(ActionError::Refused("no active workspace".into()));
    };
    let actor = state.local_user_for_room(&workspace.room);
    let may_administer = workspace.may_govern_at(
        &actor,
        tilo_action_policy::roles::governance::GRANT,
        tilo_clock::unix_now(),
    );
    let (workspace_id, room) = (workspace.id, workspace.room);
    drop(tree);
    if !may_administer {
        return Err(ActionError::Refused(format!(
            "{actor} may not change this workspace's identity"
        )));
    }
    Ok((workspace_id, room, actor))
}

fn require_explicit_confirmation(args: &ActionArgs, action: &str) -> Result<(), ActionError> {
    if args.boolean("confirm") == Some(true) {
        Ok(())
    } else {
        Err(ActionError::Refused(format!(
            "explicit confirmation is required to {action}"
        )))
    }
}

fn name_invited_peer(state: &ChromeState, args: &ActionArgs) -> Result<String, ActionError> {
    let peer = args.text("peer").unwrap_or_default().trim().to_string();
    let name = args.text("name").unwrap_or_default().trim().to_string();
    if peer.is_empty() {
        return Err(ActionError::Refused(
            "this invite needs the collaborator's device identity".into(),
        ));
    }
    if name.is_empty() {
        return Err(ActionError::Refused(
            "an invite has to say what to call them".into(),
        ));
    }
    let user = state.active_local_user();
    let named = {
        let mut tree = state.tree_mut();
        let Some(room) = tree
            .desktop
            .active_workspace()
            .map(|workspace| workspace.room)
        else {
            return Err(ActionError::Refused(
                "there is no workspace here to invite anyone into".into(),
            ));
        };
        tree.name_peer(room, &user, &peer, &name)
    };
    if !named {
        return Err(ActionError::Refused(format!(
            "{user} cannot name this collaborator in the current workspace — either \
             this device does not administer it, or `{peer}` is not a device identity"
        )));
    }
    Ok(name)
}

fn parse_device_key(value: &str) -> Result<[u8; 32], ActionError> {
    let value = value.trim();
    if value.len() != 64 {
        return Err(ActionError::Refused(
            "device fingerprint must be 64 hex characters".into(),
        ));
    }
    let mut key = [0_u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        let start = index.saturating_mul(2);
        let end = start.saturating_add(2);
        let chunk = value
            .get(start..end)
            .ok_or_else(|| ActionError::Refused("device fingerprint is not hex".into()))?;
        *slot = u8::from_str_radix(chunk, 16)
            .map_err(|_| ActionError::Refused("device fingerprint is not hex".into()))?;
    }
    Ok(key)
}

/// What `peer.invite.named` must be told. Both required — an invite to
/// nobody in particular is [`peer.invite`].
const PEER_INVITE_NAMED_PARAMS: &[ActionParam] = &[
    ActionParam::required("peer", ParamType::Text, "The invitee's device identity."),
    ActionParam::required(
        "name",
        ParamType::Text,
        "What to call them in this workspace. Shown beside their cursor and caret.",
    ),
];

/// Workspace membership and invitations, under the canonical ids and
/// parameter names of `tilo_action_broker::collaboration_admin`. The shell
/// adds `confirm` to each (its door requires an explicit confirmation for
/// privileged, sensitive and destructive actions) and an optional
/// `recipient` to `invitation.issue`; `invitation_params_follow_the_canonical_definitions`
/// keeps the rest identical.
mod membership_actions {
    use super::{ActionParam, ParamType};
    pub(super) use tilo_action_broker::collaboration_admin::{
        CANCEL_ACTION, ISSUE_ACTION, LIST_ACTION, REMOVE_ACTION, SET_ROLE_ACTION,
    };

    /// Return a pending invitation's original token again. Shell-only: the
    /// canonical surface has no resend.
    pub(super) const RESEND_ACTION: &str = "invitation.resend";

    /// The invitation actions, which run through the peer-sync network.
    pub(super) const INVITATION_ACTION_IDS: &[&str] =
        &[ISSUE_ACTION, CANCEL_ACTION, LIST_ACTION, RESEND_ACTION];

    /// Parameters the shell declares beyond the canonical ones.
    pub(super) const SHELL_ONLY_PARAMS: &[&str] = &["confirm", "recipient"];

    /// The relay refuses an invitation that lives longer than this.
    pub(super) const MAX_INVITATION_LIFETIME_SECONDS: u64 = 30 * 24 * 60 * 60;

    /// Longest display name an invitation may record.
    pub(super) const MAX_INVITEE_NAME_CHARS: usize = 80;

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

    pub(super) const MEMBER_ROLE_SET_PARAMS: &[ActionParam] = &[PARTICIPANT, ROLE, CONFIRM];
    pub(super) const MEMBER_REMOVE_PARAMS: &[ActionParam] = &[PARTICIPANT, CONFIRM];
    pub(super) const INVITATION_ISSUE_PARAMS: &[ActionParam] = &[
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
    pub(super) const INVITATION_ID_PARAMS: &[ActionParam] = &[INVITE_ID, CONFIRM];
    pub(super) const INVITATION_LIST_PARAMS: &[ActionParam] = &[CONFIRM];
}

use membership_actions::{
    CANCEL_ACTION as INVITATION_CANCEL_ID, INVITATION_ACTION_IDS,
    ISSUE_ACTION as INVITATION_ISSUE_ID, LIST_ACTION as INVITATION_LIST_ID,
    REMOVE_ACTION as MEMBER_REMOVE_ID, RESEND_ACTION as INVITATION_RESEND_ID,
    SET_ROLE_ACTION as MEMBER_ROLE_SET_ID,
};

/// The shell's contract for a membership or invitation action: permission,
/// risk and result exactly as the canonical definition declares, with the
/// shell door's explicit confirmation and at-most-once retry (this
/// transport carries no caller-supplied deduplication key).
fn collaboration_contract(id: &str) -> Option<tilo_actions::ActionContract> {
    let canonical = match id {
        INVITATION_ISSUE_ID | INVITATION_RESEND_ID => INVITATION_ISSUE_ID,
        INVITATION_CANCEL_ID | INVITATION_LIST_ID | MEMBER_ROLE_SET_ID | MEMBER_REMOVE_ID => id,
        _ => return None,
    };
    let mut contract = tilo_action_broker::collaboration_admin::action_definitions()
        .into_iter()
        .find(|definition| definition.id == canonical)?
        .contract;
    contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
    contract.retry = tilo_actions::RetryPolicy::AtMostOnce;
    Some(contract)
}

/// Decode a membership or invitation action's arguments with the canonical
/// parser, so the shell accepts exactly what every other door accepts.
fn collaboration_operation(
    canonical_id: &str,
    args: &ActionArgs,
) -> Result<tilo_action_broker::collaboration_admin::CollaborationAdminOperation, ActionError> {
    let mut json = args.to_json();
    for shell_only in membership_actions::SHELL_ONLY_PARAMS {
        json.remove(*shell_only);
    }
    tilo_action_broker::collaboration_admin::parse_operation(
        canonical_id,
        Some(&serde_json::Value::Object(json).to_string()),
    )
    .map_err(ActionError::Refused)
}

fn participant(id: &str) -> Result<crate::shell::state::UserId, ActionError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(ActionError::Refused(
            "participant_id must not be empty".into(),
        ));
    }
    Ok(id.to_string())
}

fn cap_role(
    role: tilo_action_broker::collaboration_admin::CollaborationRole,
) -> crate::shell::state::CapRole {
    use tilo_action_broker::collaboration_admin::CollaborationRole;
    match role {
        CollaborationRole::Viewer => crate::shell::state::CapRole::Viewer,
        CollaborationRole::Commenter => crate::shell::state::CapRole::Commenter,
        CollaborationRole::Editor => crate::shell::state::CapRole::Editor,
    }
}

fn invite_role(
    role: tilo_action_broker::collaboration_admin::CollaborationRole,
) -> crate::network::invite::InviteRole {
    use tilo_action_broker::collaboration_admin::CollaborationRole;
    match role {
        CollaborationRole::Viewer => crate::network::invite::InviteRole::Viewer,
        CollaborationRole::Commenter => crate::network::invite::InviteRole::Commenter,
        CollaborationRole::Editor => crate::network::invite::InviteRole::Editor,
    }
}

/// `member.role.set`: change an existing member's role.
fn apply_member_role_set(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    use tilo_action_broker::collaboration_admin::CollaborationAdminOperation;
    let CollaborationAdminOperation::SetRole(input) =
        collaboration_operation(MEMBER_ROLE_SET_ID, args)?
    else {
        return Err(ActionError::Refused(
            "member.role.set takes a participant and a role".into(),
        ));
    };
    let member = participant(&input.participant_id)?;
    crate::shell::membership::set_member_role(st, &member, cap_role(input.role)).map_err(Into::into)
}

/// `member.remove`: remove a member and revoke what they held.
fn apply_member_remove(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    use tilo_action_broker::collaboration_admin::CollaborationAdminOperation;
    let CollaborationAdminOperation::Remove(input) =
        collaboration_operation(MEMBER_REMOVE_ID, args)?
    else {
        return Err(ActionError::Refused(
            "member.remove takes a participant".into(),
        ));
    };
    let member = participant(&input.participant_id)?;
    crate::shell::membership::remove_member(st, &member).map_err(Into::into)
}

/// A 32-byte key written as 64 hex characters.
fn parse_hex_key(value: &str, what: &str) -> Result<[u8; 32], ActionError> {
    let refused = || ActionError::Refused(format!("{what} must be 64 hex characters"));
    let bytes = hex::decode(value.trim()).map_err(|_| refused())?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| refused())
}

/// The peer-sync request an invitation action asks for, validated before
/// anything is queued.
fn invitation_request(
    id: &str,
    args: &ActionArgs,
) -> Result<crate::shell::overlay_host::PeerSyncRequest, ActionError> {
    use crate::shell::overlay_host::{InvitationTerms, PeerSyncRequest};
    use tilo_action_broker::collaboration_admin::CollaborationAdminOperation as Operation;
    // Resend names an invitation exactly as cancel does.
    let canonical = if id == INVITATION_RESEND_ID {
        INVITATION_CANCEL_ID
    } else {
        id
    };
    match (id, collaboration_operation(canonical, args)?) {
        (INVITATION_ISSUE_ID, Operation::Issue(input)) => {
            if !(1..=membership_actions::MAX_INVITATION_LIFETIME_SECONDS)
                .contains(&input.expires_in_seconds)
            {
                return Err(ActionError::Refused(format!(
                    "expires_in_seconds must be between 1 and {}",
                    membership_actions::MAX_INVITATION_LIFETIME_SECONDS
                )));
            }
            let invitee_name = input
                .invitee_name
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty());
            if invitee_name.as_ref().is_some_and(|name| {
                name.chars().count() > membership_actions::MAX_INVITEE_NAME_CHARS
            }) {
                return Err(ActionError::Refused(format!(
                    "invitee_name is longer than {} characters",
                    membership_actions::MAX_INVITEE_NAME_CHARS
                )));
            }
            let recipient_user_public_key = args
                .text("recipient")
                .map(str::trim)
                .filter(|recipient| !recipient.is_empty())
                .map(|recipient| {
                    let key = recipient
                        .strip_prefix("tilo-identity:")
                        .unwrap_or(recipient);
                    parse_hex_key(key, "recipient")
                })
                .transpose()?;
            Ok(PeerSyncRequest::IssueInvitation(InvitationTerms {
                role: invite_role(input.role),
                expires_in_seconds: input.expires_in_seconds,
                invitee_name,
                recipient_user_public_key,
            }))
        }
        (INVITATION_CANCEL_ID, Operation::Cancel(input)) => Ok(PeerSyncRequest::CancelInvitation(
            parse_hex_key(&input.invite_id, "invite_id")?,
        )),
        (INVITATION_RESEND_ID, Operation::Cancel(input)) => Ok(PeerSyncRequest::ResendInvitation(
            parse_hex_key(&input.invite_id, "invite_id")?,
        )),
        (INVITATION_LIST_ID, Operation::List) => Ok(PeerSyncRequest::ListPendingInvitations),
        _ => Err(ActionError::Refused(format!(
            "{id} is not an invitation action"
        ))),
    }
}

fn invitation_requires_lifecycle_dispatch(
    _: &mut ChromeState,
    _: &ActionArgs,
) -> Result<(), ActionError> {
    Err(ActionError::Refused(
        "invitation actions require lifecycle dispatch".into(),
    ))
}

/// Poll a tracked peer-sync request until the overlay host publishes its
/// terminal result. A presentation request answers `Null`; a network
/// request answers the action's output.
fn peer_sync_ticket(chrome: Chrome, request_id: u64) -> crate::shell::search::PendingInvocation {
    Rc::new(move || {
        match chrome
            .borrow_mut()
            .take_peer_sync_request_result(request_id)
        {
            None => Ok(None),
            Some(Ok(serde_json::Value::Null)) => {
                Ok(Some(crate::shell::search::ActionReceipt::completed()))
            }
            Some(Ok(value)) => Ok(Some(crate::shell::search::ActionReceipt::completed_with(
                value,
            ))),
            Some(Err(error)) => Err(ActionError::Refused(error)),
        }
    })
}

/// Parameters of `window.focus-nth`.
/// What `tab.open-beside` may be told: which app, and what it opens.
const TAB_OPEN_BESIDE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "app",
        ParamType::Text,
        "The installed app's package id, such as `so.tilo.browser`.",
    ),
    ActionParam::optional(
        "target",
        ParamType::Text,
        "A URI the app declares, such as `https://example.org/`. Omit for an empty instance.",
    ),
    ActionParam::optional(
        "background",
        ParamType::Boolean,
        "Open behind the focused tab instead of in front of it. Defaults to false.",
    ),
];

const WINDOW_FOCUS_NTH_PARAMS: &[ActionParam] = &[ActionParam::required(
    "index",
    ParamType::Integer,
    "Which window group of the focused strip, counting from 1 (1 to 9).",
)];

/// Relative window-management actions that move focus or the view.
const WINDOW_FOCUS_ACTION_IDS: &[&str] = &[
    "window.focus-previous",
    "window.focus-next",
    "strip.focus-previous",
    "strip.focus-next",
    "workspace.previous",
    "workspace.next",
    "tab.previous",
    "tab.next",
    "window.focus-nth",
];

/// Relative window-management actions that rearrange the desk.
const WINDOW_LAYOUT_ACTION_IDS: &[&str] = &[
    "group.move-left",
    "group.move-right",
    "group.move-to-strip-previous",
    "group.move-to-strip-next",
    "group.move-to-display-previous",
    "group.move-to-display-next",
    "group.maximize",
    "group.fullscreen",
    "group.narrower",
    "group.wider",
    "tab.move-to-group-previous",
    "tab.move-to-group-next",
];

/// Queue `action` for the overlay host, which runs it on the focused group
/// exactly as its chord does.
fn request_window_shortcut(
    st: &mut ChromeState,
    action: ShortcutAction,
) -> Result<(), ActionError> {
    active_workspace_id(st)?;
    st.request_shortcut_action(action);
    Ok(())
}

/// Store an appearance change the action `action` made, then apply it.
fn persist_appearance(
    st: &mut ChromeState,
    action: &str,
    write: settings_store::SectionWrite,
) -> Result<(), ActionError> {
    write
        .commit_by(&settings_store::ChangeOrigin::action(action))
        .map_err(|err| ActionError::Refused(err.to_string()))?;
    crate::shell::i18n::apply_configured_locale();
    st.note_appearance_change();
    Ok(())
}

/// Dark or light mode, keeping whichever theme the person chose for it.
/// Choosing a mode leaves high contrast.
fn set_mode(st: &mut ChromeState, action: &str, mode: &str) -> Result<(), ActionError> {
    use settings_store::appearance::{HIGH_CONTRAST, MODE};
    let write = settings_store::SectionWrite::new()
        .set(&MODE, &Some(mode.to_string()))
        .set(&HIGH_CONTRAST, &false);
    persist_appearance(st, action, write)
}

/// High contrast in the current mode's polarity.
fn set_high_contrast(st: &mut ChromeState) -> Result<(), ActionError> {
    let write =
        settings_store::SectionWrite::new().set(&settings_store::appearance::HIGH_CONTRAST, &true);
    persist_appearance(st, "appearance.high-contrast", write)
}

fn set_language(st: &mut ChromeState, requested: &str) -> Result<(), ActionError> {
    let language = match requested.trim() {
        "en" | "en-US" | "english" | "English" => "en-US",
        "de" | "de-DE" | "german" | "German" | "deutsch" | "Deutsch" => "de-DE",
        other => {
            return Err(ActionError::Refused(format!(
                "{other:?} is not a language this desktop has"
            )));
        }
    };
    let write = settings_store::SectionWrite::new().set(
        &settings_store::appearance::LANGUAGE,
        &Some(language.to_string()),
    );
    persist_appearance(st, "appearance.language", write)
}

/// One action row. `id` is the palette command id (`overview.toggle`,
/// `settings.open`, …); `apply` runs it when the user picks the row (or an
/// agent invokes its address), taking the bound arguments and the chrome —
/// a nullary toggle simply ignores what it doesn't use.
#[derive(Clone)]
struct Action {
    id: &'static str,
    /// Catalogue key of the row's title, resolved when the row is built.
    label: &'static str,
    /// Catalogue key of the row's subtitle.
    subtitle: &'static str,
    /// Empty for everything that only needs to be told to happen.
    params: &'static [ActionParam],
    apply: fn(&mut ChromeState, &ActionArgs) -> Result<(), ActionError>,
}

/// A chrome toggle: no parameters, and nothing that can fail.
const fn chrome(
    id: &'static str,
    label: &'static str,
    subtitle: &'static str,
    apply: fn(&mut ChromeState, &ActionArgs) -> Result<(), ActionError>,
) -> Action {
    Action {
        id,
        label,
        subtitle,
        params: &[],
        apply,
    }
}

impl Action {
    const ALL: &'static [Action] = &[
        chrome(
            "dictation.toggle",
            "shortcut-name-dictation-toggle",
            "dictation-action-start-detail",
            |st, _| request_window_shortcut(st, ShortcutAction::DictationToggle),
        ),
        chrome(
            "dictation.finish",
            "shortcut-name-dictation-finish",
            "dictation-action-finish-detail",
            |st, _| request_window_shortcut(st, ShortcutAction::DictationFinish),
        ),
        chrome(
            "dictation.cancel",
            "shortcut-name-dictation-cancel",
            "dictation-action-cancel-detail",
            |st, _| request_window_shortcut(st, ShortcutAction::DictationCancel),
        ),
        chrome(
            "navigation.axis.enable",
            "search-action-axis-enable",
            "search-action-axis-enable-subtitle",
            |st, _| {
                st.set_axis_navigation_enabled(true);
                Ok(())
            },
        ),
        chrome(
            "navigation.axis.disable",
            "search-action-axis-disable",
            "search-action-axis-disable-subtitle",
            |st, _| {
                st.set_axis_navigation_enabled(false);
                Ok(())
            },
        ),
        chrome(
            "navigation.axis.gather",
            "search-action-axis-gather",
            "search-action-axis-gather-subtitle",
            |st, _| {
                if !st.axis_navigation_enabled() {
                    return Err(ActionError::Refused(t("axis-navigation-disabled")));
                }
                active_workspace_id(st)?;
                st.request_axis_gather();
                Ok(())
            },
        ),
        chrome(
            "topbar.editor.open",
            "search-action-topbar-editor",
            "search-action-topbar-editor-subtitle",
            |st, _| {
                st.request_topbar_editor();
                Ok(())
            },
        ),
        chrome(
            "overview.toggle",
            "search-action-overview-toggle",
            "search-action-overview-toggle-subtitle",
            |st, _| {
                st.overlays.overview = !st.overlays.overview;
                Ok(())
            },
        ),
        chrome(
            "settings.open",
            "search-action-settings-open",
            "search-action-settings-open-subtitle",
            |st, _| {
                st.open_workspace_settings(tilo_desktop::workspace_settings::WsTab::General);
                Ok(())
            },
        ),
        chrome(
            "settings.members.open",
            "search-action-settings-members-open",
            "search-action-settings-members-open-subtitle",
            |st, _| {
                st.open_workspace_settings(tilo_desktop::workspace_settings::WsTab::Members);
                Ok(())
            },
        ),
        chrome(
            "settings.privacy.open",
            "search-action-settings-privacy-open",
            "search-action-settings-privacy-open-subtitle",
            |st, _| {
                st.open_workspace_settings(tilo_desktop::workspace_settings::WsTab::Permissions);
                Ok(())
            },
        ),
        chrome(
            "settings.apps.open",
            "search-action-settings-apps-open",
            "search-action-settings-apps-open-subtitle",
            |st, _| {
                st.open_workspace_settings(tilo_desktop::workspace_settings::WsTab::Apps);
                Ok(())
            },
        ),
        Action {
            id: "identity.connect",
            label: "search-action-identity-connect",
            subtitle: "search-action-identity-connect-subtitle",
            params: IDENTITY_CONNECT_PARAMS,
            apply: |st, args| {
                require_explicit_confirmation(args, "connect this identity")?;
                let path = args.text("path").unwrap_or_default().trim();
                if path.is_empty() {
                    return Err(ActionError::Refused("identity path is required".into()));
                }
                let (workspace_id, room, actor) = active_admin_workspace(st)?;
                let store = crate::identity_registry::IdentityRegistryStore::from_env()
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let profile = store
                    .import(std::path::Path::new(path))
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let identity_paths = crate::firstboot::collaboration_identity_paths();
                crate::firstboot::ensure_device_certificate_for_record(
                    &profile.record_path,
                    &identity_paths,
                )
                .map_err(|error| ActionError::Refused(error.to_string()))?;
                crate::device_fleet::DeviceFleetStore::at(store.identity_dir(), identity_paths)
                    .register_current_device(&profile.id)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let enrolled = st.tree().requires_native_owner(room);
                let delegated = if enrolled {
                    Some(())
                } else {
                    let mut tree = st.tree_mut();
                    delegate_room_to_identity(
                        &mut tree,
                        workspace_id,
                        &actor,
                        &profile.principal_id(),
                    )
                };
                if delegated.is_none() {
                    return Err(ActionError::Refused(
                        "could not delegate workspace administration to the imported identity"
                            .into(),
                    ));
                }
                store
                    .bind_workspace(&room.to_string(), &profile.id)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                fence_native_selection(st, workspace_id, room, &profile.user_public_key)?;
                st.tree().export_fs_capabilities();
                st.bump();
                Ok(())
            },
        },
        Action {
            id: "identity.select",
            label: "search-action-identity-select",
            subtitle: "search-action-identity-select-subtitle",
            params: IDENTITY_SELECT_PARAMS,
            apply: |st, args| {
                require_explicit_confirmation(args, "select this identity")?;
                let identity_id = args.text("id").unwrap_or_default().trim();
                let (workspace_id, room, actor) = active_admin_workspace(st)?;
                let store = crate::identity_registry::IdentityRegistryStore::from_env()
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let registry = store
                    .load_reconciled()
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let profile = registry.identities.get(identity_id).ok_or_else(|| {
                    ActionError::Refused("that identity is not connected to this device".into())
                })?;
                if !st.tree().requires_native_owner(room) {
                    let mut tree = st.tree_mut();
                    if delegate_room_to_identity(
                        &mut tree,
                        workspace_id,
                        &actor,
                        &profile.principal_id(),
                    )
                    .is_none()
                    {
                        return Err(ActionError::Refused(
                            "could not delegate workspace administration to that identity".into(),
                        ));
                    }
                }
                store
                    .bind_workspace(&room.to_string(), identity_id)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                fence_native_selection(st, workspace_id, room, &profile.user_public_key)?;
                st.tree().export_fs_capabilities();
                st.bump();
                Ok(())
            },
        },
        chrome(
            "identity.devices.copy",
            "search-action-identity-devices-copy",
            "search-action-identity-devices-copy-subtitle",
            |st, _| {
                let (_, profile) = active_identity(st)?;
                let fleet = crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.load(&profile.id))
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                let lines = fleet
                    .certificates
                    .iter()
                    .map(|cert| {
                        let fingerprint = hex::encode(cert.device_public_key);
                        format!(
                            "{}  {}  {}",
                            cert.device_label,
                            fingerprint,
                            if fleet.is_revoked(&cert.device_public_key) {
                                "revoked"
                            } else {
                                "active"
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ui::text_selection::clipboard_copy(&lines);
                st.push_notification(
                    t("search-action-device-list-copied"),
                    t_args(
                        "search-action-device-list-copied-body",
                        &[("count", fleet.certificates.len().into())],
                    ),
                );
                Ok(())
            },
        ),
        chrome(
            "identity.device.enroll",
            "search-action-identity-device-enroll",
            "search-action-identity-device-enroll-subtitle",
            |st, _| {
                let (_, profile) = active_identity(st)?;
                let now = tilo_clock::unix_now_ns();
                let expires = now.saturating_add(5 * 60 * 1_000_000_000);
                let token = crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| {
                        store.mint_enrollment(
                            &profile.id,
                            tilo_user_identity::DeviceCapabilities::member(),
                            now,
                            expires,
                        )
                    })
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                ui::text_selection::clipboard_copy(&token);
                st.push_notification(
                    t("search-action-enrollment-copied"),
                    t("search-action-enrollment-copied-body"),
                );
                Ok(())
            },
        ),
        chrome(
            "identity.home-peer.enroll",
            "search-action-identity-home-peer-enroll",
            "search-action-identity-home-peer-enroll-subtitle",
            |st, _| {
                let (_, profile) = active_identity(st)?;
                let now = tilo_clock::unix_now_ns();
                let expires = now.saturating_add(5 * 60 * 1_000_000_000);
                let token = crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| {
                        store.mint_enrollment(
                            &profile.id,
                            tilo_user_identity::DeviceCapabilities::host(),
                            now,
                            expires,
                        )
                    })
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                ui::text_selection::clipboard_copy(&token);
                st.push_notification(
                    t("search-action-home-server-copied"),
                    t("search-action-home-server-copied-body"),
                );
                Ok(())
            },
        ),
        Action {
            id: "identity.device.revoke",
            label: "search-action-identity-device-revoke",
            subtitle: "search-action-identity-device-revoke-subtitle",
            params: DEVICE_REVOKE_PARAMS,
            apply: |st, args| {
                if args.boolean("confirm") != Some(true) {
                    return Err(ActionError::Refused(
                        "explicit confirmation is required to revoke this device".into(),
                    ));
                }
                let (_, profile) = active_identity(st)?;
                let key = parse_device_key(args.text("device").unwrap_or_default())?;
                let reason = match args
                    .text("reason")
                    .unwrap_or("retired")
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "lost" => tilo_user_identity::RevocationReason::Lost,
                    "stolen" => tilo_user_identity::RevocationReason::Stolen,
                    "retired" => tilo_user_identity::RevocationReason::Retired,
                    "compromised" => tilo_user_identity::RevocationReason::Compromised,
                    _ => {
                        return Err(ActionError::Refused(
                            "reason must be lost, stolen, retired, or compromised".into(),
                        ));
                    }
                };
                let now = tilo_clock::unix_now_ns();
                crate::device_fleet::DeviceFleetStore::from_env()
                    .and_then(|store| store.revoke(&profile.id, key, reason, now))
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                // Revoking this machine must immediately withdraw any
                // device-key filesystem grants exported for it.
                st.tree().export_fs_capabilities();
                st.push_notification(
                    t("search-action-device-revoked"),
                    t("search-action-device-revoked-body"),
                );
                Ok(())
            },
        },
        chrome(
            "quick-settings.open",
            "search-action-quick-settings-open",
            "search-action-quick-settings-open-subtitle",
            |st, _| {
                st.open_quick_settings();
                Ok(())
            },
        ),
        chrome(
            "quick-settings.clipboard",
            "search-action-quick-settings-clipboard",
            "search-action-quick-settings-clipboard-subtitle",
            |st, _| {
                st.open_quick_settings_level(crate::shell::quick_settings::QsLevel::Clipboard);
                Ok(())
            },
        ),
        chrome(
            "quick-settings.devices",
            "search-action-quick-settings-devices",
            "search-action-quick-settings-devices-subtitle",
            |st, _| {
                st.open_quick_settings_level(crate::shell::quick_settings::QsLevel::Devices);
                Ok(())
            },
        ),
        chrome(
            "projection.open",
            "search-action-projection-open",
            "search-action-projection-open-subtitle",
            |st, _| {
                st.overlays.projection = true;
                Ok(())
            },
        ),
        chrome(
            DISPLAY_EXTEND_ID,
            "search-action-display-layout-extend",
            "search-action-display-layout-extend-subtitle",
            |st, _| stage_display_layout(st, DisplayLayoutMode::Extend),
        ),
        chrome(
            DISPLAY_MIRROR_ID,
            "search-action-display-layout-mirror",
            "search-action-display-layout-mirror-subtitle",
            |st, _| stage_display_layout(st, DisplayLayoutMode::Mirror),
        ),
        chrome(
            DISPLAY_BUILTIN_ONLY_ID,
            "search-action-display-layout-builtin-only",
            "search-action-display-layout-builtin-only-subtitle",
            |st, _| stage_display_layout(st, DisplayLayoutMode::BuiltinOnly),
        ),
        chrome(
            DISPLAY_EXTERNAL_ONLY_ID,
            "search-action-display-layout-external-only",
            "search-action-display-layout-external-only-subtitle",
            |st, _| stage_display_layout(st, DisplayLayoutMode::ExternalOnly),
        ),
        chrome(
            DISPLAY_KEEP_ID,
            "search-action-display-change-keep",
            "search-action-display-change-keep-subtitle",
            |_, _| settle_display_change(true),
        ),
        chrome(
            DISPLAY_REVERT_ID,
            "search-action-display-change-revert",
            "search-action-display-change-revert-subtitle",
            |_, _| settle_display_change(false),
        ),
        chrome(
            BLUETOOTH_PAIRING_DECLINE_ID,
            "search-action-bluetooth-pairing-decline",
            "search-action-bluetooth-pairing-decline-subtitle",
            |_, _| decline_bluetooth_pairing(),
        ),
        chrome(
            "capture.open",
            "search-action-capture-open",
            "search-action-capture-open-subtitle",
            |st, _| {
                st.overlays.capture = true;
                Ok(())
            },
        ),
        Action {
            id: "capture.screenshot.send",
            label: "search-action-capture-screenshot-send",
            subtitle: "search-action-capture-screenshot-send-subtitle",
            params: SCREENSHOT_SEND_PARAMS,
            apply: |st, args| {
                let target = args.text("target").unwrap_or_default().trim();
                if target.is_empty() {
                    return Err(ActionError::Refused(
                        "name `workspace` or one of your online devices".into(),
                    ));
                }
                st.push_screenshot_send(if target == "workspace" {
                    ui::widget::ScreenshotSendTarget::Workspace
                } else {
                    ui::widget::ScreenshotSendTarget::Device(target.to_string())
                });
                Ok(())
            },
        },
        chrome(
            "collaborators.open",
            "search-action-collaborators-open",
            "search-action-collaborators-open-subtitle",
            |st, _| {
                st.overlays.collaborators = true;
                Ok(())
            },
        ),
        Action {
            id: "presence.follow",
            label: "search-action-presence-follow",
            subtitle: "search-action-presence-follow-subtitle",
            params: PRESENCE_FOLLOW_PARAMS,
            apply: |st, args| {
                let id = args.text("collaborator").unwrap_or_default().trim();
                let followable = tilo_desktop::collaborators::collaborator_roster_rows(
                    &st.tree().desktop,
                    &crate::shell::state::local_user_id(),
                )
                .into_iter()
                .any(|row| row.id == id && row.followable);
                if !followable {
                    return Err(ActionError::Refused(format!(
                        "`{id}` is not a connected, followable collaborator in this workspace"
                    )));
                }
                st.push_presence_request(crate::shell::chrome_state::PresenceRequest::Follow(
                    id.to_string(),
                ));
                Ok(())
            },
        },
        chrome(
            "presence.follow.stop",
            "search-action-presence-follow-stop",
            "search-action-presence-follow-stop-subtitle",
            |st, _| {
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::StopFollowing,
                );
                Ok(())
            },
        ),
        Action {
            id: "presence.status.set",
            label: "search-action-presence-status-set",
            subtitle: "search-action-presence-status-set-subtitle",
            params: PRESENCE_STATUS_PARAMS,
            apply: |st, args| {
                let text = args.text("text").unwrap_or_default().trim();
                if text.chars().count() > ui::live_collaboration::MAX_STATUS_CHARS {
                    return Err(ActionError::Refused(format!(
                        "status must be at most {} characters",
                        ui::live_collaboration::MAX_STATUS_CHARS
                    )));
                }
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::SetManualStatus(
                        (!text.is_empty()).then(|| text.to_string()),
                    ),
                );
                Ok(())
            },
        },
        Action {
            id: "presence.status-sharing.set",
            label: "search-action-presence-status-sharing-set",
            subtitle: "search-action-presence-status-sharing-set-subtitle",
            params: PRESENCE_SHARING_PARAMS,
            apply: |st, args| {
                let mode = match args
                    .text("mode")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "off" => ui::StatusSharing::Off,
                    "manual" | "manual-only" => ui::StatusSharing::ManualOnly,
                    "automatic" => ui::StatusSharing::Automatic,
                    _ => {
                        return Err(ActionError::Refused(
                            "status privacy must be off, manual, or automatic".into(),
                        ));
                    }
                };
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::SetStatusSharing(mode),
                );
                Ok(())
            },
        },
        chrome(
            "room.activity.open",
            "search-action-room-activity-open",
            "search-action-room-activity-open-subtitle",
            |st, _| {
                st.overlays.room_activity = true;
                // The sheet covers the dropdown that can also open it.
                st.overlays.collaborators = false;
                Ok(())
            },
        ),
        // Ranked like any other action, not appended after the hits — an
        // un-ranked row would lose "invite" to a file with the word in its name.
        chrome(
            "peer.invite",
            "search-action-peer-invite",
            "search-action-peer-invite-subtitle",
            |st, _| {
                st.push_peer_sync_request(crate::shell::overlay_host::PeerSyncAction::PromptInvite);
                Ok(())
            },
        ),
        chrome(
            "peer.connect",
            "search-action-peer-connect",
            "search-action-peer-connect-subtitle",
            |st, _| {
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::PromptConnect,
                );
                Ok(())
            },
        ),
        // The name must come from the inviter, not the invitee: a peer
        // announcing its own name could claim any name and have it render
        // next to a caret this room's capabilities gate. Recorded by the
        // inviter into the room document, the same place capability grants
        // live; the peer id stays the identity, the name is just a label.
        Action {
            id: "peer.invite.named",
            label: "search-action-peer-invite-named",
            subtitle: "search-action-peer-invite-named-subtitle",
            params: PEER_INVITE_NAMED_PARAMS,
            apply: |st, args| {
                let name = name_invited_peer(st, args)?;
                // Records the name, then opens the invite composer prefilled
                // with it for the person to review. Nothing is minted here:
                // `invitation.issue` is the action that mints a token.
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::IssueInviteFor {
                        name,
                        copy: false,
                    },
                );
                Ok(())
            },
        },
        // Workspace membership. Each passes the shell action gate and then
        // `crate::shell::membership`, the function the Members tab calls.
        Action {
            id: MEMBER_ROLE_SET_ID,
            label: "search-action-member-role-set",
            subtitle: "search-action-member-role-set-subtitle",
            params: membership_actions::MEMBER_ROLE_SET_PARAMS,
            apply: apply_member_role_set,
        },
        Action {
            id: MEMBER_REMOVE_ID,
            label: "search-action-member-remove",
            subtitle: "search-action-member-remove-subtitle",
            params: membership_actions::MEMBER_REMOVE_PARAMS,
            apply: apply_member_remove,
        },
        // Invitations run through the peer-sync network on the overlay
        // host's frame pump, so they are dispatched as pending invocations
        // (see `build_catalogue`); `apply` is never their entry point.
        Action {
            id: INVITATION_ISSUE_ID,
            label: "search-action-invitation-issue",
            subtitle: "search-action-invitation-issue-subtitle",
            params: membership_actions::INVITATION_ISSUE_PARAMS,
            apply: invitation_requires_lifecycle_dispatch,
        },
        Action {
            id: INVITATION_LIST_ID,
            label: "search-action-invitation-list",
            subtitle: "search-action-invitation-list-subtitle",
            params: membership_actions::INVITATION_LIST_PARAMS,
            apply: invitation_requires_lifecycle_dispatch,
        },
        Action {
            id: INVITATION_RESEND_ID,
            label: "search-action-invitation-resend",
            subtitle: "search-action-invitation-resend-subtitle",
            params: membership_actions::INVITATION_ID_PARAMS,
            apply: invitation_requires_lifecycle_dispatch,
        },
        Action {
            id: INVITATION_CANCEL_ID,
            label: "search-action-invitation-cancel",
            subtitle: "search-action-invitation-cancel-subtitle",
            params: membership_actions::INVITATION_ID_PARAMS,
            apply: invitation_requires_lifecycle_dispatch,
        },
        // Alongside the peer rows because the OS treats an agent as an
        // ordinary participant, but not the same operation: a peer presents
        // a signed token; an agent is admitted by the room recording a
        // capability naming it — no token, no prompt, just the grant.
        Action {
            id: "agent.invite",
            label: "search-action-agent-invite",
            subtitle: "search-action-agent-invite-subtitle",
            params: AGENT_INVITE_PARAMS,
            apply: |st, args| {
                use tilo_action_policy::roles::AgentPreset;
                let name = args.text("name").unwrap_or(DEFAULT_AGENT_NAME).trim();
                let name = if name.is_empty() {
                    DEFAULT_AGENT_NAME
                } else {
                    name
                };
                // Case-folded: `Scribe` and `scribe` are otherwise two
                // different subjects, half-admitting the agent.
                let agent = name.to_lowercase();
                let preset = match args.text("preset") {
                    Some(value) => AgentPreset::parse(value).ok_or_else(|| {
                        ActionError::Refused(
                            "choose Observer, Contributor, Operator, or Custom".into(),
                        )
                    })?,
                    None => AgentPreset::Observer,
                };
                let user = st.active_local_user();
                let granted = {
                    let mut tree = st.tree_mut();
                    let Some((ws_id, room, held)) = tree
                        .desktop
                        .active_workspace()
                        .map(|w| (w.id, w.room.to_string(), w.policy.grants.clone()))
                    else {
                        return Err(ActionError::Refused(
                            "there is no workspace here to add an agent to".into(),
                        ));
                    };
                    // Ordinary grants on the room, through the tree so they
                    // reach the room document (and other members) and disk.
                    let changes = preset.admission(
                        &room,
                        &agent,
                        &tilo_action_policy::Subject::person(user.clone()),
                        &held,
                    );
                    tree.govern_in_workspace(ws_id, &user, changes).ok()
                };
                if granted.is_none() {
                    return Err(ActionError::Refused(format!(
                        "{user} does not administer this workspace, so {agent} was not added"
                    )));
                }
                // Land back on the dedicated, clearly non-human roster. The
                // next visible action is Review access, not a chat prompt.
                st.open_workspace_settings(crate::shell::chrome_state::WsTab::Agents);
                Ok(())
            },
        },
        chrome(
            "tab.new.terminal.here",
            "search-action-tab-new-terminal-here",
            "search-action-tab-new-terminal-here-subtitle",
            |st, _| {
                crate::shell::window_actions::open_terminal(
                    st,
                    crate::shell::window_actions::Destination::Here,
                );
                Ok(())
            },
        ),
        chrome(
            "window.new.terminal",
            "search-action-window-new-terminal",
            "search-action-window-new-terminal-subtitle",
            |st, _| {
                crate::shell::window_actions::open_terminal(
                    st,
                    crate::shell::window_actions::Destination::Beside,
                );
                Ok(())
            },
        ),
        chrome(
            "tab.duplicate",
            "search-action-tab-duplicate",
            "search-action-tab-duplicate-subtitle",
            |st, _| {
                crate::shell::window_actions::duplicate_active_tab(st).map_or_else(
                    |err| {
                        let message = err.user_message();
                        st.push_warning_notification(t("host-tab-duplicate-failed"), &message);
                        Err(ActionError::Refused(message))
                    },
                    |_| Ok(()),
                )
            },
        ),
        // Another instance of an app as a tab right after the focused one:
        // what an app's "open in new tab" asks for, open to every caller.
        Action {
            id: "tab.open-beside",
            label: "search-action-tab-open-beside",
            subtitle: "search-action-tab-open-beside-subtitle",
            params: TAB_OPEN_BESIDE_PARAMS,
            apply: |st, args| {
                let app = args.text("app").unwrap_or_default().trim().to_owned();
                if app.is_empty() {
                    return Err(ActionError::Refused(
                        "an app is required; nothing was opened".into(),
                    ));
                }
                let target = args
                    .text("target")
                    .map(str::trim)
                    .filter(|target| !target.is_empty())
                    .map(str::to_owned);
                let background = args.boolean("background").unwrap_or(false);
                let anchor = crate::shell::window_actions::active_tab_id(st)
                    .and_then(|tab| crate::shell::app_tabs::anchor_for_tab(&st.tree(), tab));
                crate::shell::app_tabs::open_beside(st, &app, target.as_deref(), anchor, background)
                    .map_err(|error| ActionError::Refused(error.to_string()))
            },
        },
        // The tab menu's three rows as actions, on the focused tab: a
        // capability reachable only by right-click is not finished. The
        // rows and these closures call the same `window_actions` functions.
        chrome(
            "tab.menu",
            "search-action-tab-menu",
            "search-action-tab-menu-subtitle",
            |st, _| {
                if st.session_locked() || crate::shell::window_actions::active_tab_id(st).is_none()
                {
                    return Err(ActionError::Refused(t(
                        "search-action-tab-menu-unavailable",
                    )));
                }
                request_window_shortcut(st, ShortcutAction::TabMenu)
            },
        ),
        chrome(
            "tab.drag.cancel",
            "search-action-tab-drag-cancel",
            "search-action-tab-drag-cancel-subtitle",
            |st, _| {
                if st.session_locked() {
                    return Err(ActionError::Refused(t(
                        "search-action-tab-drag-cancel-unavailable",
                    )));
                }
                st.request_tab_drag_cancel();
                Ok(())
            },
        ),
        chrome(
            "tab.close",
            "search-action-tab-close",
            "search-action-tab-close-subtitle",
            |st, _| {
                crate::shell::window_actions::close_active_tab(st).map_err(|err| {
                    let message = err.user_message();
                    st.push_warning_notification(t("search-action-tab-close-failed"), &message);
                    ActionError::Refused(message)
                })
            },
        ),
        chrome(
            "tab.reopen",
            "search-action-tab-reopen",
            "search-action-tab-reopen-subtitle",
            |st, _| {
                st.reopen_closed_tab().map(|_| ()).ok_or_else(|| {
                    ActionError::Refused("no closed tab to reopen in this workspace".into())
                })
            },
        ),
        chrome(
            "tab.split-right",
            "search-action-tab-split-right",
            "search-action-tab-split-right-subtitle",
            |st, _| {
                crate::shell::window_actions::split_active_tab(
                    st,
                    crate::shell::window_actions::SplitPlacement::Right,
                )
                .map(|_| ())
                .map_err(|err| {
                    let message = err.user_message();
                    st.push_warning_notification(t("search-action-tab-split-failed"), &message);
                    ActionError::Refused(message)
                })
            },
        ),
        chrome(
            "window.focus-previous",
            "search-action-window-focus-previous",
            "search-action-window-focus-previous-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::FocusPreviousWindow),
        ),
        chrome(
            "window.focus-next",
            "search-action-window-focus-next",
            "search-action-window-focus-next-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::FocusNextWindow),
        ),
        chrome(
            "strip.focus-previous",
            "search-action-strip-focus-previous",
            "search-action-strip-focus-previous-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::PreviousStrip),
        ),
        chrome(
            "strip.focus-next",
            "search-action-strip-focus-next",
            "search-action-strip-focus-next-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::NextStrip),
        ),
        chrome(
            "workspace.previous",
            "search-action-workspace-previous",
            "search-action-workspace-previous-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::PreviousWorkspace),
        ),
        chrome(
            "workspace.next",
            "search-action-workspace-next",
            "search-action-workspace-next-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::NextWorkspace),
        ),
        chrome(
            "tab.previous",
            "search-action-tab-previous",
            "search-action-tab-previous-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::PreviousTab),
        ),
        chrome(
            "tab.next",
            "search-action-tab-next",
            "search-action-tab-next-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::NextTab),
        ),
        chrome(
            "group.move-left",
            "search-action-group-move-left",
            "search-action-group-move-left-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::MoveGroupLeft),
        ),
        chrome(
            "group.move-right",
            "search-action-group-move-right",
            "search-action-group-move-right-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::MoveGroupRight),
        ),
        chrome(
            "group.move-to-strip-previous",
            "search-action-group-move-to-strip-previous",
            "search-action-group-move-to-strip-previous-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::MoveGroupToPreviousStrip),
        ),
        chrome(
            "group.move-to-strip-next",
            "search-action-group-move-to-strip-next",
            "search-action-group-move-to-strip-next-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::MoveGroupToNextStrip),
        ),
        chrome(
            "group.move-to-display-previous",
            "search-action-group-move-to-display-previous",
            "search-action-group-move-to-display-previous-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::MoveGroupToPreviousDisplay),
        ),
        chrome(
            "group.move-to-display-next",
            "search-action-group-move-to-display-next",
            "search-action-group-move-to-display-next-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::MoveGroupToNextDisplay),
        ),
        chrome(
            "group.maximize",
            "search-action-group-maximize",
            "search-action-group-maximize-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::ToggleMaximize),
        ),
        chrome(
            "group.fullscreen",
            "search-action-group-fullscreen",
            "search-action-group-fullscreen-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::ToggleFullscreen),
        ),
        chrome(
            "group.narrower",
            "search-action-group-narrower",
            "search-action-group-narrower-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::ResizeNarrower),
        ),
        chrome(
            "group.wider",
            "search-action-group-wider",
            "search-action-group-wider-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::ResizeWider),
        ),
        chrome(
            "tab.move-to-group-previous",
            "search-action-tab-move-to-group-previous",
            "search-action-tab-move-to-group-previous-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::AttachTabLeft),
        ),
        chrome(
            "tab.move-to-group-next",
            "search-action-tab-move-to-group-next",
            "search-action-tab-move-to-group-next-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::AttachTabRight),
        ),
        Action {
            id: "window.focus-nth",
            label: "search-action-window-focus-nth",
            subtitle: "search-action-window-focus-nth-subtitle",
            params: WINDOW_FOCUS_NTH_PARAMS,
            apply: |st, args| {
                let action = args
                    .integer("index")
                    .and_then(|index| usize::try_from(index).ok()?.checked_sub(1))
                    .and_then(|index| {
                        ShortcutAction::ALL
                            .into_iter()
                            .find(|action| action.focus_group_index() == Some(index))
                    })
                    .ok_or_else(|| ActionError::Refused("index must be 1 to 9".into()))?;
                request_window_shortcut(st, action)
            },
        },
        chrome(
            "tab.move-to-new-group",
            "search-action-tab-move-to-new-group",
            "search-action-tab-move-to-new-group-subtitle",
            |st, _| {
                crate::shell::window_actions::split_active_tab(
                    st,
                    crate::shell::window_actions::SplitPlacement::StripEnd,
                )
                .map(|_| ())
                .map_err(|err| {
                    let message = err.user_message();
                    st.push_warning_notification(t("host-tab-move-failed"), &message);
                    ActionError::Refused(message)
                })
            },
        ),
        chrome(
            "notifications.dnd.toggle",
            "search-action-notifications-dnd-toggle",
            "search-action-notifications-dnd-toggle-subtitle",
            |st, _| {
                st.toggle_dnd();
                Ok(())
            },
        ),
        chrome(
            "appearance.theme.dark",
            "search-action-appearance-theme-dark",
            "search-action-appearance-theme-dark-subtitle",
            |st, _| set_mode(st, "appearance.theme.dark", "dark"),
        ),
        chrome(
            "appearance.theme.light",
            "search-action-appearance-theme-light",
            "search-action-appearance-theme-light-subtitle",
            |st, _| set_mode(st, "appearance.theme.light", "light"),
        ),
        chrome(
            "appearance.high-contrast",
            "search-action-appearance-high-contrast",
            "search-action-appearance-high-contrast-subtitle",
            |st, _| set_high_contrast(st),
        ),
        Action {
            id: "appearance.language",
            label: "search-action-appearance-language",
            subtitle: "search-action-appearance-language-subtitle",
            params: LANGUAGE_PARAMS,
            apply: |st, args| {
                let language = args.text("language").unwrap_or_default();
                if language.trim().is_empty() {
                    return Err(ActionError::Refused(
                        "a language is required; nothing was changed".into(),
                    ));
                }
                set_language(st, language)
            },
        },
        // The palette does this on Enter over a record row; this is the
        // address for a caller that names a record itself.
        Action {
            id: "record.bring",
            label: "search-action-record-bring",
            subtitle: "search-action-record-bring-subtitle",
            params: RECORD_BRING_PARAMS,
            apply: |st, args| {
                let id = args.text("id").unwrap_or_default();
                if id.trim().is_empty() {
                    return Err(ActionError::Refused(
                        "a record id is required; nothing was brought here".into(),
                    ));
                }
                if ui::record::RecordId::parse(id).is_none() {
                    return Err(ActionError::Refused(format!(
                        "{id:?} is not a record id this build can read"
                    )));
                }
                let kind = args.text("kind").unwrap_or("document");
                let kind = if kind.trim().is_empty() {
                    "document"
                } else {
                    kind
                };
                let tree = st.tree();
                let workspace = tree.desktop.active_workspace().ok_or_else(|| {
                    ActionError::Refused("no active workspace accepts the record".into())
                })?;
                let user = st.local_user_for_room(&workspace.room);
                let actor = workspace.actor_for_person(&user);
                let target = (workspace.id, workspace.room, actor);
                drop(tree);
                st.push_tracked_record_request(id, kind, target.0, target.1, target.2);
                Ok(())
            },
        },
        // Last: an English "open" on a German desktop matches every *.open
        // id at the same score, so declaration order is the only tiebreak.
        // The tour and the help overlay must not win that race.
        chrome(
            "shortcut-help.toggle",
            "search-action-shortcut-help-toggle",
            "search-action-shortcut-help-toggle-subtitle",
            |st, _| {
                st.overlays.shortcut_help = !st.overlays.shortcut_help;
                Ok(())
            },
        ),
        chrome(
            "desktop-tour.open",
            "search-action-desktop-tour-open",
            "search-action-desktop-tour-open-subtitle",
            |st, _| {
                st.overlays.desktop_tour = true;
                Ok(())
            },
        ),
    ];
}

pub(crate) struct SharedQsDispatcher {
    actions: std::cell::RefCell<Rc<dyn crate::shell::quick_settings::actions::QsActions>>,
    availability: std::cell::Cell<crate::shell::quick_settings::actions::QsActionAvailability>,
    radio_job: std::cell::RefCell<Option<crate::shell::quick_settings::radio::RadioTicket>>,
    radio_wake: std::cell::RefCell<Option<crate::session_runtime::Wake>>,
}

impl SharedQsDispatcher {
    pub(crate) fn new(
        actions: Rc<dyn crate::shell::quick_settings::actions::QsActions>,
        availability: crate::shell::quick_settings::actions::QsActionAvailability,
    ) -> Self {
        Self {
            actions: std::cell::RefCell::new(actions),
            availability: std::cell::Cell::new(availability),
            radio_job: std::cell::RefCell::new(None),
            radio_wake: std::cell::RefCell::new(None),
        }
    }

    pub(crate) fn replace(
        &self,
        actions: Rc<dyn crate::shell::quick_settings::actions::QsActions>,
        availability: crate::shell::quick_settings::actions::QsActionAvailability,
    ) {
        if let Some(job) = self.radio_job.borrow().as_ref() {
            job.retire();
        }
        *self.actions.borrow_mut() = actions;
        self.availability.set(availability);
    }

    pub(crate) fn set_radio_wake(&self, wake: crate::session_runtime::Wake) {
        *self.radio_wake.borrow_mut() = Some(wake);
    }

    pub(crate) fn observe_radio_availability(&self, radios: &os_actions::RadioState) {
        let mut availability = self.availability.get();
        availability.wifi = radios.wifi_available;
        availability.airplane_mode = radios.wifi_available || radios.wwan_available;
        self.availability.set(availability);
    }

    pub(crate) fn start_radio(
        &self,
        change: os_actions::RadioChange,
        chrome: &Chrome,
    ) -> Result<Option<crate::shell::quick_settings::radio::RadioTicket>, String> {
        let id = match change {
            os_actions::RadioChange::Wifi(_) => QS_WIFI_ID,
            os_actions::RadioChange::Airplane(_) => QS_AIRPLANE_ID,
        };
        if !self.action_available(id) {
            return Err("radio action is unavailable".into());
        }
        if self.radio_job.borrow().is_some() {
            return Err("a radio change is already pending".into());
        }
        let actions = self.actions.borrow().clone();
        if let Some(work) = actions.prepare_radio_change(change) {
            let job = crate::shell::quick_settings::radio::RadioTicket::start(
                change,
                work,
                self.radio_wake.borrow().clone(),
            )?;
            *self.radio_job.borrow_mut() = Some(job.clone());
            let mut state = chrome.borrow_mut();
            state.quick_settings.radio_change_pending = true;
            state.bump();
            Ok(Some(job))
        } else {
            let result = change.execute(actions.as_ref());
            result?;
            crate::shell::quick_settings::radio::apply(change, Ok(()), chrome);
            Ok(None)
        }
    }

    pub(crate) fn take_radio_completion(
        &self,
    ) -> Option<crate::shell::quick_settings::radio::RadioTicket> {
        let mut pending = self.radio_job.borrow_mut();
        if !pending.as_ref()?.finished() {
            return None;
        }
        let _ = pending.as_ref()?.result()?;
        pending.take()
    }

    /// What the Quick Settings rows are built from; see [`QuickSettingsKey`].
    fn catalogue_key(&self) -> QuickSettingsKey {
        QuickSettingsKey {
            backend: Rc::as_ptr(&*self.actions.borrow()).cast::<()>().addr(),
            availability: self.availability.get(),
            radio_pending: self.radio_job.borrow().is_some(),
        }
    }

    fn action_available(&self, id: &str) -> bool {
        let available = self.availability.get();
        match id {
            QS_BRIGHTNESS_ID => available.brightness,
            QS_VOLUME_ID => available.volume,
            QS_WIFI_ID => available.wifi,
            QS_POWER_ID => available.power_profile,
            QS_VPN_ID => available.vpn,
            QS_BLUETOOTH_ID => available.bluetooth,
            QS_AIRPLANE_ID => available.airplane_mode,
            _ => false,
        }
    }
}

pub(crate) type SharedQsActions = Rc<SharedQsDispatcher>;

pub(crate) const QS_BRIGHTNESS_ID: &str = "quick-settings.brightness.set";
pub(crate) const QS_VOLUME_ID: &str = "quick-settings.volume.set";
pub(crate) const QS_WIFI_ID: &str = "quick-settings.wifi.set-enabled";
pub(crate) const QS_POWER_ID: &str = "quick-settings.power-profile.set";
pub(crate) const QS_VPN_ID: &str = "quick-settings.vpn.set-enabled";
pub(crate) const QS_BLUETOOTH_ID: &str = "quick-settings.bluetooth.set-enabled";
pub(crate) const QS_AIRPLANE_ID: &str = "quick-settings.airplane-mode.set-enabled";

fn quick_settings_params(id: &str) -> Vec<ActionParam> {
    let percent = || {
        ActionParam::required_owned(
            "percent",
            ParamType::Number,
            "Exact percentage from 0 through 100.",
        )
        .with_schema(serde_json::json!({"minimum": 0, "maximum": 100}))
    };
    match id {
        QS_BRIGHTNESS_ID => vec![
            percent(),
            ActionParam::optional_owned(
                "display",
                ParamType::Text,
                "Connector of the monitor to set, e.g. eDP-1 or DP-2, as wl_output names it. \
                 Omitted: the built-in panel, else the first monitor.",
            )
            .with_schema(serde_json::json!({"minLength": 1, "maxLength": 64})),
        ],
        QS_VOLUME_ID => vec![percent()],
        QS_WIFI_ID => vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether the Wi-Fi radio should be enabled.",
        )],
        QS_BLUETOOTH_ID => vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether the Bluetooth radio should be powered.",
        )],
        QS_AIRPLANE_ID => vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether airplane mode should turn the wireless radios off.",
        )],
        QS_POWER_ID => vec![
            ActionParam::required_owned("profile", ParamType::Text, "The host power profile.")
                .with_choices(["saver", "balanced", "performance"]),
        ],
        QS_VPN_ID => vec![
            ActionParam::required_owned(
                "name",
                ParamType::Text,
                "Exact configured VPN connection name.",
            )
            .with_schema(serde_json::json!({"minLength": 1, "maxLength": 255})),
            ActionParam::required_owned(
                "enabled",
                ParamType::Boolean,
                "Whether the named VPN connection should be enabled.",
            ),
        ],
        _ => Vec::new(),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "validated hardware percentages are bounded to 0..=100 before this f64-to-f32 provider conversion"
)]
fn validated_percent(args: &ActionArgs) -> f32 {
    args.number("percent").unwrap_or_default() as f32
}

fn apply_quick_settings_effect(
    id: &str,
    actions: &dyn crate::shell::quick_settings::actions::QsActions,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    let result = match id {
        QS_BRIGHTNESS_ID => match args.text("display") {
            Some(display) => actions.set_display_brightness(display, validated_percent(args)),
            None => actions.set_brightness(validated_percent(args)),
        },
        QS_VOLUME_ID => actions.set_volume(validated_percent(args)),
        QS_WIFI_ID => actions.set_wifi(args.boolean("enabled").unwrap_or(false)),
        QS_BLUETOOTH_ID => actions.set_bluetooth(args.boolean("enabled").unwrap_or(false)),
        QS_AIRPLANE_ID => actions.set_airplane_mode(args.boolean("enabled").unwrap_or(false)),
        QS_POWER_ID => {
            let profile = match args.text("profile") {
                Some("saver") => crate::shell::quick_settings::actions::PowerProfile::Saver,
                Some("balanced") => crate::shell::quick_settings::actions::PowerProfile::Balanced,
                Some("performance") => {
                    crate::shell::quick_settings::actions::PowerProfile::Performance
                }
                _ => return Err(ActionError::Refused("invalid power profile".into())),
            };
            actions.set_power_profile(profile)
        }
        QS_VPN_ID => actions.set_vpn_connection(
            args.text("name").unwrap_or_default(),
            args.boolean("enabled").unwrap_or(false),
        ),
        _ => return Err(ActionError::Refused("unknown Quick Settings effect".into())),
    };
    result.map_err(ActionError::Refused)
}

/// UI adapter for the callable Quick Settings slice. The root panel and its
/// drill-ins still own presentation state, while these effects bind the
/// same typed parameters and call the same provider path as external action
/// invocation. Operations outside this slice pass through unchanged.
pub(crate) struct ValidatedQsActions<'a> {
    inner: &'a dyn crate::shell::quick_settings::actions::QsActions,
}

impl<'a> ValidatedQsActions<'a> {
    pub(crate) fn new(inner: &'a dyn crate::shell::quick_settings::actions::QsActions) -> Self {
        Self { inner }
    }

    fn dispatch(
        &self,
        id: &str,
        values: impl IntoIterator<Item = (String, crate::shell::search::action_api::ArgValue)>,
    ) -> Result<(), String> {
        let args = ActionArgs::bind(&quick_settings_params(id), values)
            .map_err(|error| error.to_string())?;
        apply_quick_settings_effect(id, self.inner, &args).map_err(|error| error.to_string())
    }
}

impl crate::shell::quick_settings::actions::QsActions for ValidatedQsActions<'_> {
    fn prepare_radio_change(
        &self,
        change: os_actions::RadioChange,
    ) -> Option<os_actions::RadioWork> {
        self.inner.prepare_radio_change(change)
    }

    fn set_brightness(&self, percent: f32) -> Result<(), String> {
        self.dispatch(
            QS_BRIGHTNESS_ID,
            [(
                "percent".into(),
                crate::shell::search::action_api::ArgValue::Number(f64::from(percent)),
            )],
        )
    }

    fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String> {
        self.dispatch(
            QS_BRIGHTNESS_ID,
            [
                (
                    "percent".into(),
                    crate::shell::search::action_api::ArgValue::Number(f64::from(percent)),
                ),
                (
                    "display".into(),
                    crate::shell::search::action_api::ArgValue::Text(connector.to_owned()),
                ),
            ],
        )
    }

    fn set_volume(&self, percent: f32) -> Result<(), String> {
        self.dispatch(
            QS_VOLUME_ID,
            [(
                "percent".into(),
                crate::shell::search::action_api::ArgValue::Number(f64::from(percent)),
            )],
        )
    }

    fn set_wifi(&self, enabled: bool) -> Result<(), String> {
        self.dispatch(
            QS_WIFI_ID,
            [(
                "enabled".into(),
                crate::shell::search::action_api::ArgValue::Boolean(enabled),
            )],
        )
    }

    fn set_power_profile(
        &self,
        profile: crate::shell::quick_settings::actions::PowerProfile,
    ) -> Result<(), String> {
        let profile = match profile {
            crate::shell::quick_settings::actions::PowerProfile::Saver => "saver",
            crate::shell::quick_settings::actions::PowerProfile::Balanced => "balanced",
            crate::shell::quick_settings::actions::PowerProfile::Performance => "performance",
        };
        self.dispatch(
            QS_POWER_ID,
            [(
                "profile".into(),
                crate::shell::search::action_api::ArgValue::Text(profile.into()),
            )],
        )
    }

    fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
        self.dispatch(
            QS_VPN_ID,
            [
                (
                    "name".into(),
                    crate::shell::search::action_api::ArgValue::Text(name.into()),
                ),
                (
                    "enabled".into(),
                    crate::shell::search::action_api::ArgValue::Boolean(enabled),
                ),
            ],
        )
    }

    fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        self.inner.connect_wifi(ssid, password)
    }

    fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
        self.dispatch(
            QS_BLUETOOTH_ID,
            [(
                "enabled".into(),
                crate::shell::search::action_api::ArgValue::Boolean(enabled),
            )],
        )
    }

    fn connect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.inner.connect_bluetooth(address)
    }

    fn disconnect_bluetooth(&self, address: &str) -> Result<(), String> {
        self.inner.disconnect_bluetooth(address)
    }

    fn set_wired(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_wired(enabled)
    }

    fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
        self.dispatch(
            QS_AIRPLANE_ID,
            [(
                "enabled".into(),
                crate::shell::search::action_api::ArgValue::Boolean(enabled),
            )],
        )
    }

    fn set_hotspot(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_hotspot(enabled)
    }

    fn set_mobile_broadband(&self, enabled: bool) -> Result<(), String> {
        self.inner.set_mobile_broadband(enabled)
    }

    fn toggle_mute(&self) -> Result<(), String> {
        self.inner.toggle_mute()
    }

    fn set_mute(&self, muted: bool) -> Result<(), String> {
        self.inner.set_mute(muted)
    }

    fn brightness(&self) -> Result<f32, String> {
        self.inner.brightness()
    }

    fn keyboard_backlight(&self) -> Result<os_actions::KeyboardBacklight, String> {
        self.inner.keyboard_backlight()
    }

    fn set_keyboard_backlight(&self, level: u32) -> Result<(), String> {
        self.inner.set_keyboard_backlight(level)
    }
}

pub(crate) const DISPLAY_EXTEND_ID: &str = "display.layout.extend";
pub(crate) const DISPLAY_MIRROR_ID: &str = "display.layout.mirror";
pub(crate) const DISPLAY_BUILTIN_ONLY_ID: &str = "display.layout.builtin-only";
pub(crate) const DISPLAY_EXTERNAL_ONLY_ID: &str = "display.layout.external-only";
pub(crate) const DISPLAY_KEEP_ID: &str = "display.change.keep";
pub(crate) const DISPLAY_REVERT_ID: &str = "display.change.revert";
pub(crate) const BLUETOOTH_PAIRING_DECLINE_ID: &str = "bluetooth.pairing.decline";

/// Decline the Bluetooth pairing question on screen, or stop the pairing
/// whose code it displays.
///
/// There is deliberately no accepting counterpart. Confirming a pairing
/// means comparing a code with the device in front of you, or typing the one
/// it shows; only the person at this machine can do that, on the dialog
/// itself. An agent or a script can always say no, and never yes.
fn decline_bluetooth_pairing() -> Result<(), ActionError> {
    if crate::bluetooth::PairingPromptHandle::global().dismiss() {
        Ok(())
    } else {
        Err(ActionError::Refused(
            "no Bluetooth pairing is waiting".into(),
        ))
    }
}

/// Stage a whole-desktop layout through the same confirm-or-restore
/// transaction as Super+P, and open the display sheet so whoever is at the
/// machine sees Keep/Revert and the countdown.
fn stage_display_layout(
    _st: &mut ChromeState,
    _mode: DisplayLayoutMode,
) -> Result<(), ActionError> {
    Err(ActionError::Refused(
        "display layouts require lifecycle dispatch".into(),
    ))
}

/// Keep or revert the display change that is waiting for confirmation.
fn settle_display_change(keep: bool) -> Result<(), ActionError> {
    use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};

    let mut transaction = DisplayTransaction::load()
        .ok_or_else(|| ActionError::Refused("no display change is waiting".into()))?;
    let waiting = if keep {
        transaction.state == DisplayTransactionState::AwaitingConfirmation
    } else {
        matches!(
            transaction.state,
            DisplayTransactionState::Requested
                | DisplayTransactionState::AwaitingConfirmation
                | DisplayTransactionState::ConfirmRequested
        )
    };
    if !waiting {
        return Err(ActionError::Refused("no display change is waiting".into()));
    }
    if keep {
        transaction.request_confirmation()
    } else {
        transaction.request_revert()
    }
    .map_err(|error| ActionError::Refused(error.to_string()))
}

fn display_layout_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        retry: tilo_actions::RetryPolicy::Idempotent,
        // An unconfirmed layout restores itself; Revert undoes it at once.
        reversible: true,
        ..tilo_actions::ActionContract::default()
    }
}

fn quick_settings_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        retry: tilo_actions::RetryPolicy::Idempotent,
        reversible: false,
        ..tilo_actions::ActionContract::default()
    }
}

fn quick_settings_contract_for(available: bool) -> tilo_actions::ActionContract {
    let mut contract = quick_settings_contract();
    if !available {
        contract.availability = tilo_actions::Availability::Unavailable {
            code: "quick_settings_unavailable".into(),
            reason: t("search-action-quick-settings-unavailable"),
        };
    }
    contract
}

pub struct ActionsProvider {
    qs_actions: SharedQsActions,
    /// The rows' searchable text, indexed while it stays the same.
    ranker: ListIndex,
    search_preferences: Option<crate::shell::search::SearchPreferencesHandle>,
    provider_catalog: Option<crate::shell::search::SearchProviderCatalogHandle>,
    /// Every row this provider offers, built once per state they are built
    /// from rather than once per keystroke or lookup.
    catalogue: std::cell::RefCell<Option<Rc<Catalogue>>>,
    /// How many times the catalogue has been built.
    #[cfg(test)]
    builds: Rc<std::cell::Cell<usize>>,
}

/// The shell's action rows for one [`CatalogueKey`], before any query
/// narrows them.
struct Catalogue {
    key: CatalogueKey,
    rows: Vec<SearchResult>,
    /// Each row's searchable text, in row order.
    fields: Vec<Vec<(String, FieldWeight)>>,
    /// The first row with each id.
    by_id: std::collections::HashMap<String, usize>,
    /// The per-source on/off switches, offered only to a query about search
    /// itself ([`asks_about_search_sources`]).
    source_switches: std::ops::Range<usize>,
}

/// Everything the catalogue's rows are built from: whether a row exists,
/// what it says, its contract, its parameters and what its callbacks hold.
/// Invocation still rechecks authority against the live workspace.
#[derive(Clone, PartialEq, Eq)]
struct CatalogueKey {
    state: CatalogueState,
    /// Whose ownership and role actions the governance rows offer.
    actor: crate::shell::state::UserId,
}

/// The part of [`CatalogueKey`] that is cheap to read on every palette
/// refresh, and so also the provider's search revision.
#[derive(Clone, PartialEq, Eq, Hash)]
struct CatalogueState {
    chrome: u64,
    /// Workspaces, their names and governance: the chrome bumps this on every
    /// tree mutation it makes...
    chrome_revision: u64,
    /// ...and mutations from outside the chrome bump this, until the next
    /// tick folds them into `chrome_revision`.
    external_tree_revision: u64,
    locale: String,
    peer_sync: bool,
    /// Whether this device can issue an invitation into the active workspace
    /// ([`crate::shell::invite_policy`]): the `peer.invite` rows are offered
    /// only then, as the invite composer is.
    invite_offered: bool,
    collab: bool,
    active_workspace: Option<crate::shell::state::WorkspaceId>,
    quick_settings: QuickSettingsKey,
    /// The search sources and whether each is on, for their switches.
    search_sources: Vec<(String, String, bool)>,
}

/// The Quick Settings backend the rows call, what it can do, and whether a
/// radio change is pending. The backend is named by address: the rows hold
/// it, so it cannot be freed and another take its place while they live.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct QuickSettingsKey {
    backend: usize,
    availability: crate::shell::quick_settings::actions::QsActionAvailability,
    radio_pending: bool,
}

impl ActionsProvider {
    pub fn new() -> Self {
        Self {
            search_preferences: None,
            provider_catalog: None,
            qs_actions: Rc::new(SharedQsDispatcher::new(
                Rc::new(crate::shell::quick_settings::actions::UnavailableQsActions),
                crate::shell::quick_settings::actions::QsActionAvailability::default(),
            )),
            ranker: ListIndex::default(),
            catalogue: std::cell::RefCell::default(),
            #[cfg(test)]
            builds: Rc::default(),
        }
    }

    pub(crate) fn with_all_sources(
        search_preferences: crate::shell::search::SearchPreferencesHandle,
        provider_catalog: crate::shell::search::SearchProviderCatalogHandle,
        qs_actions: SharedQsActions,
    ) -> Self {
        Self {
            search_preferences: Some(search_preferences),
            provider_catalog: Some(provider_catalog),
            qs_actions,
            ranker: ListIndex::default(),
            catalogue: std::cell::RefCell::default(),
            #[cfg(test)]
            builds: Rc::default(),
        }
    }

    pub(crate) fn with_shared_qs_actions(qs_actions: SharedQsActions) -> Self {
        Self {
            qs_actions,
            ..Self::new()
        }
    }

    pub fn with_preferences(
        search_preferences: crate::shell::search::SearchPreferencesHandle,
    ) -> Self {
        Self {
            search_preferences: Some(search_preferences),
            provider_catalog: None,
            ..Self::new()
        }
    }

    pub fn with_preferences_and_catalog(
        search_preferences: crate::shell::search::SearchPreferencesHandle,
        provider_catalog: crate::shell::search::SearchProviderCatalogHandle,
    ) -> Self {
        Self {
            search_preferences: Some(search_preferences),
            provider_catalog: Some(provider_catalog),
            ..Self::new()
        }
    }
}

impl Default for ActionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for ActionsProvider {
    fn id(&self) -> &'static str {
        "actions"
    }
    fn name(&self) -> &'static str {
        "Actions"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn available_in_active_app_scope(&self) -> bool {
        true
    }
    /// An address is looked up in the whole table. The shell offers more
    /// actions than one palette page shows, so the first page of an empty
    /// query would leave the later ones unreachable by id.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return None;
        }
        let catalogue = self.catalogue(&ctx);
        let found = match catalogue.by_id.get(id) {
            Some(&at) => catalogue.rows.get(at).cloned(),
            None => crate::shell::notification::alarm_action_rows(ctx.chrome)
                .into_iter()
                .find(|row| row.id == id),
        };
        found.map(|mut row| {
            // What an empty query scores it.
            row.score = crate::shell::search::provider_score(0);
            row
        })
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.catalogue_state(ctx).hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let catalogue = self.catalogue(&ctx);
        // Alarm actions follow the notification log, so they are read fresh.
        let alarms = crate::shell::notification::alarm_action_rows(ctx.chrome);
        let mut alarm_fields = ActionCandidates::default();
        for row in &alarms {
            alarm_fields.record(
                &row.title.to_lowercase(),
                &row.subtitle.clone().unwrap_or_default().to_lowercase(),
                &row.id,
            );
        }
        let withheld = if asks_about_search_sources(ctx.needle) {
            0..0
        } else {
            catalogue.source_switches.clone()
        };
        let offered = || {
            catalogue
                .rows
                .iter()
                .zip(&catalogue.fields)
                .enumerate()
                .filter(|(at, _)| !withheld.contains(at))
                .map(|(_, offered)| offered)
        };
        let mut ranks = self
            .ranker
            .ranks(
                ctx.needle,
                offered()
                    .map(|(_, fields)| fields)
                    .chain(&alarm_fields.0)
                    .map(|fields| fields.iter().map(|(text, weight)| (text.as_str(), *weight))),
            )
            .into_iter();
        let mut out = Vec::new();
        for (row, _) in offered() {
            if let Some(rank) = ranks.next().flatten() {
                out.push(SearchResult {
                    score: crate::shell::search::provider_score(rank),
                    ..row.clone()
                });
            }
        }
        for (mut row, rank) in alarms.into_iter().zip(ranks) {
            if let Some(rank) = rank {
                row.score = crate::shell::search::provider_score(rank);
                out.push(row);
            }
        }
        out
    }
}

impl ActionsProvider {
    /// The catalogue for the current state, built only if that is not the
    /// state the retained one was built from.
    fn catalogue(&self, ctx: &SearchContext<'_>) -> Rc<Catalogue> {
        let key = CatalogueKey {
            state: self.catalogue_state(ctx),
            actor: ctx.chrome.active_local_user(),
        };
        let retained = self.catalogue.borrow().clone();
        if let Some(catalogue) = retained.filter(|catalogue| catalogue.key == key) {
            return catalogue;
        }
        let catalogue = Rc::new(self.build_catalogue(*ctx, key));
        *self.catalogue.borrow_mut() = Some(Rc::clone(&catalogue));
        catalogue
    }

    fn catalogue_state(&self, ctx: &SearchContext<'_>) -> CatalogueState {
        let tree = ctx.chrome.tree();
        let external_tree_revision = tree.desktop.external_revision;
        let active_workspace = tree
            .desktop
            .active_workspace()
            .map(|workspace| workspace.id);
        drop(tree);
        CatalogueState {
            chrome: ctx.chrome.instance(),
            chrome_revision: ctx.chrome.revision,
            external_tree_revision,
            locale: crate::shell::i18n::current_locale(),
            peer_sync: ctx.chrome.peer_sync_enabled,
            invite_offered: ctx.chrome.peer_sync_enabled
                && crate::shell::invite_policy::active_invite_route(ctx.chrome)
                    .is_some_and(crate::shell::invite_policy::InviteRoute::issues_here),
            collab: ctx.chrome.collab_available(),
            active_workspace,
            quick_settings: self.qs_actions.catalogue_key(),
            search_sources: self.search_sources(),
        }
    }

    /// Each search source and whether it is on: what its switch says and
    /// does. Empty without the preferences the switches write.
    fn search_sources(&self) -> Vec<(String, String, bool)> {
        let (Some(preferences), Some(catalog)) = (&self.search_preferences, &self.provider_catalog)
        else {
            return Vec::new();
        };
        let preferences = preferences.borrow();
        let sources = catalog
            .borrow()
            .iter()
            .map(|(id, name)| (id.clone(), name.clone(), preferences.provider_enabled(id)))
            .collect();
        drop(preferences);
        sources
    }

    /// Build every row the provider offers, whatever the needle; see
    /// [`CatalogueKey`] for what they are built from.
    #[expect(
        clippy::too_many_lines,
        reason = "the catalog lists every shell action with its contract in one table-like pass"
    )]
    fn build_catalogue(&self, ctx: SearchContext<'_>, key: CatalogueKey) -> Catalogue {
        const QUICK_SETTINGS_ACTIONS: &[(&str, &str, &str)] = &[
            (
                QS_BRIGHTNESS_ID,
                "search-action-quick-settings-brightness-set",
                "search-action-quick-settings-brightness-set-subtitle",
            ),
            (
                QS_VOLUME_ID,
                "search-action-quick-settings-volume-set",
                "search-action-quick-settings-volume-set-subtitle",
            ),
            (
                QS_WIFI_ID,
                "search-action-quick-settings-wifi-set-enabled",
                "search-action-quick-settings-wifi-set-enabled-subtitle",
            ),
            (
                QS_POWER_ID,
                "search-action-quick-settings-power-profile-set",
                "search-action-quick-settings-power-profile-set-subtitle",
            ),
            (
                QS_VPN_ID,
                "search-action-quick-settings-vpn-set-enabled",
                "search-action-quick-settings-vpn-set-enabled-subtitle",
            ),
            (
                QS_BLUETOOTH_ID,
                "search-action-quick-settings-bluetooth-set-enabled",
                "search-action-quick-settings-bluetooth-set-enabled-subtitle",
            ),
            (
                QS_AIRPLANE_ID,
                "search-action-quick-settings-airplane-mode-set-enabled",
                "search-action-quick-settings-airplane-mode-set-enabled-subtitle",
            ),
        ];
        #[cfg(test)]
        self.builds.set(self.builds.get().wrapping_add(1));
        let mut out = Vec::new();
        // Every row is built with its searchable text; a query ranks them
        // together to keep the matches and score them.
        let mut candidates = ActionCandidates::default();
        for action in Action::ALL {
            if action.id == "topbar.editor.open"
                && ctx.chrome.fullscreen_group().is_some()
                && ctx.chrome.is_fullscreen()
            {
                continue;
            }
            // A session not running peer sync has nothing to invite into.
            if (action.id.starts_with("peer.") || INVITATION_ACTION_IDS.contains(&action.id))
                && !ctx.chrome.peer_sync_enabled
            {
                continue;
            }
            // The one invitation policy: no row for an invitation the issuer
            // would refuse from here, as there is no composer for it.
            if matches!(action.id, "peer.invite" | "peer.invite.named") && !key.state.invite_offered
            {
                continue;
            }
            // Needs a room to record the capability in and a working
            // broker (peer sync is not required — an agent uses this
            // session's own broker socket).
            if action.id.starts_with("agent.")
                && (!ctx.chrome.collab_available()
                    || ctx.chrome.tree().desktop.active_workspace().is_none())
            {
                continue;
            }
            let label = t(action.label);
            let subtitle = t(action.subtitle);
            let label_lower = label.to_lowercase();
            let subtitle_lower = subtitle.to_lowercase();
            candidates.record(&label_lower, &subtitle_lower, action.id);
            {
                let score = 0;
                let apply = action.apply;
                let (on_open, on_begin_invoke) = if action.id == "record.bring" {
                    let on_open = Rc::new(
                        |_doc: &mut ui::widget::Edits<'_>,
                         _root: &automerge::ObjId,
                         _chrome: &Chrome,
                         _args: &ActionArgs| {
                            Err(ActionError::Refused(
                                "record.bring requires lifecycle dispatch".into(),
                            ))
                        },
                    ) as Rc<crate::shell::search::OpenFn>;
                    let begin = Rc::new(
                        |_doc: &mut ui::widget::Edits<'_>,
                         _root: &automerge::ObjId,
                         chrome: &Chrome,
                         args: &ActionArgs| {
                            let id = args.text("id").unwrap_or_default();
                            let kind = args.text("kind").unwrap_or("document");
                            let (workspace_id, room, actor) = {
                                let state = chrome.borrow();
                                let tree = state.tree();
                                let Some(workspace) = tree.desktop.active_workspace() else {
                                    return Err(ActionError::Refused("no active workspace".into()));
                                };
                                let user = state.local_user_for_room(&workspace.room);
                                let actor = workspace.actor_for_person(&user);
                                if !shell_action_authorized_for(workspace, &actor, "record.bring") {
                                    return Err(ActionError::Refused(
                                        "current workspace authority no longer permits record.bring"
                                            .into(),
                                    ));
                                }
                                let located = (workspace.id, workspace.room, actor);
                                drop(tree);
                                located
                            };
                            let request_id = chrome.borrow_mut().push_tracked_record_request(
                                id.to_string(),
                                kind.to_string(),
                                workspace_id,
                                room,
                                actor,
                            );
                            let chrome = chrome.clone();
                            let ticket: crate::shell::search::PendingInvocation =
                                Rc::new(move || {
                                    match chrome.borrow_mut().take_record_request_result(request_id)
                                    {
                                        None => Ok(None),
                                        Some(Ok(())) => Ok(Some(
                                            crate::shell::search::ActionReceipt::completed(),
                                        )),
                                        Some(Err(error)) => Err(ActionError::Refused(error)),
                                    }
                                });
                            Ok(crate::shell::search::InvocationDisposition::Pending(ticket))
                        },
                    ) as Rc<crate::shell::search::BeginInvokeFn>;
                    (on_open, Some(begin))
                } else if matches!(
                    action.id,
                    "peer.invite" | "peer.connect" | "peer.invite.named"
                ) {
                    let action_id = action.id;
                    let on_open = Rc::new(
                        move |_doc: &mut ui::widget::Edits<'_>,
                              _root: &automerge::ObjId,
                              _chrome: &Chrome,
                              _args: &ActionArgs| {
                            Err(ActionError::Refused(format!(
                                "{action_id} requires lifecycle dispatch"
                            )))
                        },
                    ) as Rc<crate::shell::search::OpenFn>;
                    let begin = Rc::new(
                        move |_doc: &mut ui::widget::Edits<'_>,
                              _root: &automerge::ObjId,
                              chrome: &Chrome,
                              args: &ActionArgs| {
                            let request_id = {
                                let mut state = chrome.borrow_mut();
                                if !current_shell_action_authorized(&state, action_id) {
                                    return Err(ActionError::Refused(format!(
                                        "current workspace authority no longer permits {action_id}"
                                    )));
                                }
                                if action_id != "peer.connect"
                                    && let Some(refusal) =
                                        crate::shell::invite_policy::active_invite_route(&state)
                                            .map_or(Some("no active workspace"), |route| {
                                                route.refusal()
                                            })
                                {
                                    return Err(ActionError::Refused(refusal.into()));
                                }
                                let request = match action_id {
                                    "peer.invite" => {
                                        crate::shell::overlay_host::PeerSyncAction::PromptInvite
                                    }
                                    "peer.connect" => {
                                        crate::shell::overlay_host::PeerSyncAction::PromptConnect
                                    }
                                    "peer.invite.named" => {
                                        let name = name_invited_peer(&state, args)?;
                                        state.bump();
                                        crate::shell::overlay_host::PeerSyncAction::IssueInviteFor {
                                            name,
                                            copy: false,
                                        }
                                    }
                                    #[expect(
                                        clippy::unreachable,
                                        reason = "this closure is only built for the peer lifecycle ids matched above"
                                    )]
                                    _ => unreachable!("matched peer lifecycle action"),
                                };
                                state.begin_peer_sync_request(request)
                            };
                            Ok(crate::shell::search::InvocationDisposition::Pending(
                                peer_sync_ticket(chrome.clone(), request_id),
                            ))
                        },
                    ) as Rc<crate::shell::search::BeginInvokeFn>;
                    (on_open, Some(begin))
                } else if INVITATION_ACTION_IDS.contains(&action.id) {
                    let action_id = action.id;
                    let on_open = Rc::new(
                        move |_doc: &mut ui::widget::Edits<'_>,
                              _root: &automerge::ObjId,
                              _chrome: &Chrome,
                              _args: &ActionArgs| {
                            Err(ActionError::Refused(format!(
                                "{action_id} requires lifecycle dispatch"
                            )))
                        },
                    ) as Rc<crate::shell::search::OpenFn>;
                    let begin = Rc::new(
                        move |_doc: &mut ui::widget::Edits<'_>,
                              _root: &automerge::ObjId,
                              chrome: &Chrome,
                              args: &ActionArgs| {
                            let request = invitation_request(action_id, args)?;
                            let request_id = {
                                let mut state = chrome.borrow_mut();
                                if !current_shell_action_authorized(&state, action_id) {
                                    return Err(ActionError::Refused(format!(
                                        "current workspace authority no longer permits {action_id}"
                                    )));
                                }
                                let workspace = active_workspace_id(&state)?;
                                state.begin_peer_sync_request(
                                    crate::shell::overlay_host::PeerSyncAction::Network {
                                        workspace,
                                        request,
                                    },
                                )
                            };
                            Ok(crate::shell::search::InvocationDisposition::Pending(
                                peer_sync_ticket(chrome.clone(), request_id),
                            ))
                        },
                    ) as Rc<crate::shell::search::BeginInvokeFn>;
                    (on_open, Some(begin))
                } else if let Some(mode) = action
                    .id
                    .strip_prefix("display.layout.")
                    .and_then(DisplayLayoutMode::from_id)
                {
                    let action_id = action.id;
                    let on_open = Rc::new(
                        |_doc: &mut ui::widget::Edits<'_>,
                         _root: &automerge::ObjId,
                         _chrome: &Chrome,
                         _args: &ActionArgs| {
                            Err(ActionError::Refused(
                                "display layouts require lifecycle dispatch".into(),
                            ))
                        },
                    ) as Rc<crate::shell::search::OpenFn>;
                    let begin = Rc::new(
                        move |_doc: &mut ui::widget::Edits<'_>,
                              _root: &automerge::ObjId,
                              chrome: &Chrome,
                              _args: &ActionArgs| {
                            if !current_shell_action_authorized(&chrome.borrow(), action_id) {
                                return Err(ActionError::Refused("current workspace authority no longer permits this display layout".into()));
                            }
                            let pending = crate::shell::display_persistence::stage_layout(mode)
                                .map_err(|error| ActionError::Refused(error.to_string()))?;
                            {
                                let mut state = chrome.borrow_mut();
                                state.overlays.projection = true;
                                state.bump();
                            }
                            let chrome = chrome.clone();
                            let ticket: crate::shell::search::PendingInvocation =
                                Rc::new(move || {
                                    pending
                                        .poll()
                                        .map(|done| {
                                            done.map(|_| {
                                                chrome.borrow_mut().bump();
                                                crate::shell::search::ActionReceipt::completed()
                                            })
                                        })
                                        .map_err(|error| ActionError::Refused(error.to_string()))
                                });
                            Ok(crate::shell::search::InvocationDisposition::Pending(ticket))
                        },
                    ) as Rc<crate::shell::search::BeginInvokeFn>;
                    (on_open, Some(begin))
                } else if matches!(action.id, DISPLAY_KEEP_ID | DISPLAY_REVERT_ID) {
                    use crate::shell::display_persistence::{Answer, answer};
                    let action_id = action.id;
                    let on_open = Rc::new(
                        |_doc: &mut ui::widget::Edits<'_>,
                         _root: &automerge::ObjId,
                         _chrome: &Chrome,
                         _args: &ActionArgs| {
                            Err(ActionError::Refused(
                                "display answers require lifecycle dispatch".into(),
                            ))
                        },
                    ) as Rc<crate::shell::search::OpenFn>;
                    let begin = Rc::new(
                        move |_doc: &mut ui::widget::Edits<'_>,
                              _root: &automerge::ObjId,
                              chrome: &Chrome,
                              _args: &ActionArgs| {
                            if !current_shell_action_authorized(&chrome.borrow(), action_id) {
                                return Err(ActionError::Refused("current workspace authority no longer permits this display answer".into()));
                            }
                            let pending = answer(if action_id == DISPLAY_KEEP_ID {
                                Answer::Keep
                            } else {
                                Answer::Revert
                            })
                            .map_err(|error| ActionError::Refused(error.to_string()))?;
                            let chrome = chrome.clone();
                            let ticket: crate::shell::search::PendingInvocation =
                                Rc::new(move || {
                                    pending
                                        .poll()
                                        .map(|done| {
                                            done.map(|()| {
                                                chrome.borrow_mut().bump();
                                                crate::shell::search::ActionReceipt::completed()
                                            })
                                        })
                                        .map_err(|error| ActionError::Refused(error.to_string()))
                                });
                            Ok(crate::shell::search::InvocationDisposition::Pending(ticket))
                        },
                    ) as Rc<crate::shell::search::BeginInvokeFn>;
                    (on_open, Some(begin))
                } else {
                    let action_id = action.id;
                    let on_open = Rc::new(
                        move |_doc: &mut ui::widget::Edits<'_>,
                              _root: &automerge::ObjId,
                              chrome: &Chrome,
                              args: &ActionArgs| {
                            let mut st = chrome.borrow_mut();
                            if !current_shell_action_authorized(&st, action_id) {
                                return Err(ActionError::Refused(format!(
                                    "current workspace authority no longer permits {action_id}"
                                )));
                            }
                            let outcome = apply(&mut st, args);
                            // Repaint only on success; a refusal changed nothing.
                            if outcome.is_ok() {
                                st.bump();
                            }
                            outcome
                        },
                    ) as Rc<crate::shell::search::OpenFn>;
                    (on_open, None)
                };
                out.push(SearchResult {
                    kind: ResourceKind::Action,
                    operation_kind: shell_action_kind(action.id),
                    provider_id: self.id().to_string(),
                    id: action.id.to_string(),
                    title: label,
                    subtitle: Some(subtitle),
                    badge: None,
                    score,
                    params: action.params.to_vec(),
                    contract: Some(shell_action_contract(action.id)),
                    on_open,
                    on_invoke: None,
                    on_begin_invoke,
                });
            }
        }
        let descriptors = tilo_desktop::activity::view_actions::workspace_view_descriptors(
            &ctx.chrome.tree().desktop,
            true,
        );
        for descriptor in descriptors {
            let (subtitle, keywords) = workspace_view_copy(&descriptor.request);
            candidates.record(
                &descriptor.label.to_lowercase(),
                &format!("{subtitle} {keywords}").to_lowercase(),
                &descriptor.id,
            );
            let score = 0;
            let request = descriptor.request;
            out.push(SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::View,
                provider_id: self.id().into(),
                id: descriptor.id,
                title: descriptor.label,
                subtitle: Some(subtitle),
                badge: None,
                score,
                params: vec![],
                contract: Some(tilo_actions::ActionContract::view()),
                on_open: Rc::new(move |_, _, chrome, _| {
                    chrome
                        .borrow_mut()
                        .request_workspace_view(request.clone())
                        .map_err(ActionError::Refused)
                }),
                on_invoke: None,
                on_begin_invoke: None,
            });
        }
        for (id, label_key, subtitle_key, keywords_key) in [
            (
                tilo_desktop::activity::ACTIVITY_ACKNOWLEDGE,
                "search-action-activity-acknowledge",
                "search-action-activity-acknowledge-subtitle",
                "search-action-activity-acknowledge-keywords",
            ),
            (
                tilo_desktop::activity::ACTIVITY_TASKS,
                "search-action-activity-tasks",
                "search-action-catch-up-tasks-subtitle",
                "search-action-activity-tasks-keywords",
            ),
            (
                tilo_desktop::activity::HISTORY_COMPARE,
                "search-action-history-compare",
                "search-action-history-compare-subtitle",
                "search-action-history-compare-keywords",
            ),
            (
                tilo_desktop::activity::HISTORY_UNDO,
                "search-action-history-undo",
                "search-action-history-undo-subtitle",
                "search-action-history-undo-keywords",
            ),
            (
                tilo_desktop::activity::HISTORY_RESTORE,
                "search-action-history-restore",
                "search-action-history-restore-subtitle",
                "search-action-history-restore-keywords",
            ),
        ] {
            let label = t(label_key);
            let subtitle = t(subtitle_key);
            let keywords = t(keywords_key);
            candidates.record(
                &label.to_lowercase(),
                &format!("{subtitle} {keywords}").to_lowercase(),
                id,
            );
            out.push(SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::View,
                provider_id: self.id().into(),
                id: id.to_string(),
                title: label,
                subtitle: Some(subtitle),
                badge: None,
                score: 0,
                params: vec![],
                contract: Some(tilo_actions::ActionContract::view()),
                on_open: Rc::new(move |_, _, chrome, _| {
                    chrome
                        .borrow_mut()
                        .request_history_selection_action(id)
                        .map_err(ActionError::Refused)
                }),
                on_invoke: None,
                on_begin_invoke: None,
            });
        }
        for &(id, label_key, subtitle_key) in QUICK_SETTINGS_ACTIONS {
            let label = t(label_key);
            let subtitle = t(subtitle_key);
            candidates.record(&label.to_lowercase(), &subtitle.to_lowercase(), id);
            let score = 0;
            let actions = self.qs_actions.actions.borrow().clone();
            let available = self.qs_actions.action_available(id);
            let mut contract = quick_settings_contract_for(available);
            if available
                && matches!(id, QS_WIFI_ID | QS_AIRPLANE_ID)
                && self.qs_actions.radio_job.borrow().is_some()
            {
                contract.availability = tilo_actions::Availability::Unavailable {
                    code: "quick_settings_radio_pending".into(),
                    reason: t("display-confirm-applying"),
                };
            }
            let on_begin_invoke = matches!(id, QS_WIFI_ID | QS_AIRPLANE_ID).then(|| {
                let slot = Rc::clone(&self.qs_actions);
                Rc::new(
                    move |_: &mut ui::widget::Edits<'_>,
                          _: &automerge::ObjId,
                          chrome: &Chrome,
                          args: &ActionArgs| {
                        if !current_shell_action_authorized(&chrome.borrow(), id) {
                            return Err(ActionError::Refused(format!(
                                "current workspace authority no longer permits {id}"
                            )));
                        }
                        let enabled = args.boolean("enabled").unwrap_or(false);
                        let change = if id == QS_WIFI_ID {
                            os_actions::RadioChange::Wifi(enabled)
                        } else {
                            os_actions::RadioChange::Airplane(enabled)
                        };
                        match slot
                            .start_radio(change, chrome)
                            .map_err(ActionError::Refused)?
                        {
                            None => Ok(crate::shell::search::InvocationDisposition::Accepted(
                                crate::shell::search::ActionReceipt::completed(),
                            )),
                            Some(job) => {
                                let chrome = chrome.clone();
                                let pending: crate::shell::search::PendingInvocation =
                                    Rc::new(move || {
                                        let Some(result) = job.result() else {
                                            return Ok(None);
                                        };
                                        job.apply(&chrome);
                                        result.map_err(ActionError::Refused)?;
                                        Ok(Some(crate::shell::search::ActionReceipt::completed()))
                                    });
                                Ok(crate::shell::search::InvocationDisposition::Pending(
                                    pending,
                                ))
                            }
                        }
                    },
                ) as Rc<crate::shell::search::BeginInvokeFn>
            });
            out.push(SearchResult {
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                badge: None,
                provider_id: self.id().to_string(),
                id: id.to_string(),
                title: label,
                subtitle: Some(subtitle),
                score,
                params: quick_settings_params(id),
                contract: Some(contract),
                on_open: Rc::new(move |_doc, _root, chrome, args| {
                    if !current_shell_action_authorized(&chrome.borrow(), id) {
                        return Err(ActionError::Refused(format!(
                            "current workspace authority no longer permits {id}"
                        )));
                    }
                    if matches!(id, QS_WIFI_ID | QS_AIRPLANE_ID) {
                        return Err(ActionError::Refused(
                            "radio changes require lifecycle dispatch".into(),
                        ));
                    }
                    apply_quick_settings_effect(id, actions.as_ref(), args)?;
                    let mut state = chrome.borrow_mut();
                    match id {
                        QS_BRIGHTNESS_ID => {
                            let level = validated_percent(args) / 100.0;
                            match args.text("display") {
                                Some(display) => {
                                    if let Some(entry) = state
                                        .quick_settings
                                        .displays
                                        .iter_mut()
                                        .find(|entry| entry.connector == display)
                                    {
                                        entry.level = level;
                                    }
                                }
                                None => state.quick_settings.brightness = level,
                            }
                        }
                        QS_VOLUME_ID => {
                            state.quick_settings.volume = validated_percent(args) / 100.0;
                        }
                        QS_WIFI_ID => {
                            state.quick_settings.wifi = args.boolean("enabled") == Some(true);
                        }
                        QS_BLUETOOTH_ID => {
                            state.quick_settings.bluetooth = args.boolean("enabled") == Some(true);
                        }
                        QS_AIRPLANE_ID => {
                            state.quick_settings.radios.airplane_mode =
                                args.boolean("enabled") == Some(true);
                        }
                        QS_POWER_ID => {
                            state.quick_settings.power_profile = match args.text("profile") {
                                Some("saver") => crate::shell::quick_settings::PowerProfile::Saver,
                                Some("performance") => {
                                    crate::shell::quick_settings::PowerProfile::Performance
                                }
                                _ => crate::shell::quick_settings::PowerProfile::Balanced,
                            };
                        }
                        QS_VPN_ID => {
                            if let Some(connection) =
                                state.quick_settings.vpns.iter_mut().find(|connection| {
                                    Some(connection.name.as_str()) == args.text("name")
                                })
                            {
                                connection.active = args.boolean("enabled") == Some(true);
                            }
                        }
                        _ => {}
                    }
                    state.bump();
                    Ok(())
                }),
                on_invoke: None,
                on_begin_invoke,
            });
        }
        let actor = key.actor.clone();
        let ownership_resource = {
            let tree = ctx.chrome.tree();
            tree.desktop.active_workspace().map(|workspace| {
                tilo_desktop::workspace_settings::ownership_transfer_resource(workspace, &actor)
            })
        };
        if let Some(resource) = ownership_resource {
            for action in resource.actions {
                let subtitle = action
                    .description
                    .clone()
                    .unwrap_or_else(|| t("search-action-governance-subtitle"));
                {
                    candidates.record(
                        &action.label.to_lowercase(),
                        &subtitle.to_lowercase(),
                        &action.id,
                    );
                    let score = 0;
                    let action_id = action.id.clone();
                    let params = action.params().to_vec();
                    out.push(SearchResult {
                        kind: ResourceKind::Action,
                        operation_kind: tilo_actions::OperationKind::Document,
                        provider_id: self.id().to_string(),
                        id: format!("{}#{}", resource.id.as_str(), action.id),
                        title: action.label,
                        subtitle: Some(subtitle),
                        badge: None,
                        score,
                        params,
                        contract: Some(action.contract),
                        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, arguments| {
                            let mut state = chrome.borrow_mut();
                            apply_ownership_action(&state, &action_id, arguments)?;
                            state.bump();
                            Ok(())
                        }),
                        on_invoke: None,
                        on_begin_invoke: None,
                    });
                }
            }
        }
        let profile_resource = {
            let tree = ctx.chrome.tree();
            tree.desktop.active_workspace().map(|workspace| {
                tilo_desktop::workspace_settings::governance_resource(workspace, &actor)
            })
        };
        if let Some(resource) = profile_resource {
            let mut surfaces = vec![(resource.id.as_str().to_string(), resource.actions)];
            surfaces.extend(
                resource
                    .children
                    .into_iter()
                    .map(|child| (child.id.as_str().to_string(), child.actions)),
            );
            for (resource_id, actions) in surfaces {
                for action in actions {
                    let subtitle = action
                        .description
                        .clone()
                        .unwrap_or_else(|| t("search-action-permission-roles-subtitle"));
                    candidates.record(
                        &action.label.to_lowercase(),
                        &subtitle.to_lowercase(),
                        &action.id,
                    );
                    let score = 0;
                    let spec = action.clone();
                    let params = action.params().to_vec();
                    out.push(SearchResult {
                        kind: ResourceKind::Action,
                        operation_kind: tilo_actions::OperationKind::Document,
                        provider_id: self.id().to_string(),
                        id: format!("{}#{}", resource_id, action.id),
                        title: action.label,
                        subtitle: Some(subtitle),
                        badge: None,
                        score,
                        params,
                        contract: Some(action.contract),
                        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, arguments| {
                            let mut state = chrome.borrow_mut();
                            apply_governance_action(&state, &spec, arguments)?;
                            state.bump();
                            Ok(())
                        }),
                        on_invoke: None,
                        on_begin_invoke: None,
                    });
                }
            }
        }
        let mut source_switches = 0..0;
        if let Some(preferences) = &self.search_preferences {
            let id = "search.provider.set";
            let label = t("search-action-search-provider-set");
            let subtitle = t("search-action-search-provider-set-subtitle");
            {
                candidates.record(&label.to_lowercase(), &subtitle.to_lowercase(), id);
                let score = 0;
                let preferences = Rc::clone(preferences);
                out.push(SearchResult {
                    kind: ResourceKind::Action,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    provider_id: self.id().to_string(),
                    id: id.to_string(),
                    title: label,
                    subtitle: Some(subtitle),
                    badge: None,
                    score,
                    params: SEARCH_PROVIDER_SET_PARAMS.to_vec(),
                    contract: None,
                    on_open: Rc::new(move |_doc, _root, chrome: &Chrome, args: &ActionArgs| {
                        let provider = args.text("provider").unwrap_or_default().trim();
                        let enabled = args.boolean("enabled").unwrap_or(true);
                        if provider.is_empty() {
                            return Err(ActionError::Refused(
                                "a search provider id is required".into(),
                            ));
                        }
                        if provider == "actions" && !enabled {
                            return Err(ActionError::Refused(
                                "the Actions source cannot be disabled because it is the shared accessibility and automation door".into(),
                            ));
                        }
                        let mut next = preferences.borrow().clone();
                        next.set_provider_enabled(provider, enabled);
                        next.save().map_err(|error| {
                            ActionError::Refused(format!(
                                "could not save the search-source preference: {error}"
                            ))
                        })?;
                        *preferences.borrow_mut() = next;
                        chrome.borrow_mut().bump();
                        Ok(())
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                });
            }
            if let Some(catalog) = &self.provider_catalog {
                source_switches.start = out.len();
                for (provider_id, provider_name) in catalog.borrow().iter() {
                    if provider_id == "actions" {
                        continue;
                    }
                    let enabled = preferences.borrow().provider_enabled(provider_id);
                    let source: &[(&str, ui::i18n::ArgValue)] = &[
                        ("source", provider_name.clone().into()),
                        ("id", provider_id.clone().into()),
                    ];
                    let (label, subtitle) = if enabled {
                        (
                            t_args("search-action-provider-disable", source),
                            t_args("search-action-provider-disable-subtitle", source),
                        )
                    } else {
                        (
                            t_args("search-action-provider-enable", source),
                            t_args("search-action-provider-enable-subtitle", source),
                        )
                    };
                    let id = format!("search.provider.toggle:{provider_id}");
                    candidates.record(
                        &label.to_lowercase(),
                        &format!("{subtitle} {}", t("search-action-provider-keywords"))
                            .to_lowercase(),
                        &id,
                    );
                    let score = 0;
                    let preferences = Rc::clone(preferences);
                    let provider_id = provider_id.clone();
                    out.push(SearchResult {
                        kind: ResourceKind::Action,
                        operation_kind: tilo_actions::OperationKind::Effect,
                        provider_id: self.id().to_string(),
                        id,
                        title: label,
                        subtitle: Some(subtitle),
                        badge: Some(if enabled {
                            t("search-action-provider-badge-on")
                        } else {
                            t("search-action-provider-badge-off")
                        }),
                        score,
                        params: Vec::new(),
                        contract: None,
                        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args| {
                            let mut next = preferences.borrow().clone();
                            next.set_provider_enabled(&provider_id, !enabled);
                            next.save().map_err(|error| {
                                ActionError::Refused(format!(
                                    "could not save the search-source preference: {error}"
                                ))
                            })?;
                            *preferences.borrow_mut() = next;
                            chrome.borrow_mut().bump();
                            Ok(())
                        }),
                        on_invoke: None,
                        on_begin_invoke: None,
                    });
                }
                source_switches.end = out.len();
            }
        }
        let mut by_id = std::collections::HashMap::with_capacity(out.len());
        for (at, row) in out.iter().enumerate() {
            by_id.entry(row.id.clone()).or_insert(at);
        }
        Catalogue {
            key,
            rows: out,
            fields: candidates.0,
            by_id,
            source_switches,
        }
    }
}

/// Whether a query is about search itself, so a per-source on/off switch is
/// an answer to it. "notes" looks for notes, not for hiding them; an empty
/// listing and an exact `search.provider.toggle:…` address still see every
/// switch.
fn asks_about_search_sources(needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() || needle.starts_with("search.provider.") {
        return true;
    }
    let keywords = t("search-action-provider-keywords").to_lowercase();
    needle
        .split_whitespace()
        .filter(|word| word.chars().count() >= 3)
        .any(|word| {
            keywords
                .split_whitespace()
                .any(|keyword| keyword.starts_with(word))
        })
}

/// A workspace view's subtitle and the extra words it answers to. Switching
/// to a workspace is not an answer to "history", and each Activity view says
/// what it shows rather than that it is a view.
fn workspace_view_copy(
    request: &tilo_desktop::activity::view_actions::WorkspaceViewRequest,
) -> (String, String) {
    use tilo_desktop::activity::ActivityView;
    use tilo_desktop::activity::view_actions::WorkspaceViewRequest;
    let activity = |view: ActivityView| {
        let subtitle = match view {
            ActivityView::CatchUp => "search-action-activity-catch-up-subtitle",
            ActivityView::Tasks => "search-action-activity-tasks-subtitle",
            ActivityView::Explore => "search-action-activity-explore-subtitle",
            ActivityView::Replay => "search-action-activity-replay-subtitle",
        };
        (t(subtitle), t("search-action-activity-keywords"))
    };
    match request {
        WorkspaceViewRequest::Select { .. } => (
            t("search-action-workspace-switch-subtitle"),
            t("search-action-workspace-switch-keywords"),
        ),
        WorkspaceViewRequest::OpenActivity { view, .. } => activity(*view),
        WorkspaceViewRequest::CloseActivity { .. } => activity(ActivityView::CatchUp),
    }
}

/// A typed action id is a verbatim identity — scripts and power users name
/// actions by it — so it weighs close to the label.
pub(super) const ACTION_ID_WEIGHT: FieldWeight = FieldWeight::new(90);

/// Each action row's label, id and subtitle, in row order.
#[derive(Default)]
struct ActionCandidates(Vec<Vec<(String, FieldWeight)>>);

impl ActionCandidates {
    /// Record the searchable text of the row pushed next.
    fn record(&mut self, label: &str, subtitle: &str, id: &str) {
        self.0.push(vec![
            (label.to_owned(), FieldWeight::TITLE),
            (id.to_owned(), ACTION_ID_WEIGHT),
            (subtitle.to_owned(), FieldWeight::SUBTITLE),
        ]);
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;

/// Called only after the existing identity registry accepted the selection.
#[allow(
    clippy::similar_names,
    reason = "`room` and `root` are the protocol's own names for the room and its user root"
)]
fn fence_native_selection(
    st: &ChromeState,
    local: crate::shell::state::WorkspaceId,
    room: crate::shell::state::RoomId,
    root: &[u8; 32],
) -> Result<(), ActionError> {
    let owner = st.tree().installed_native_owner();
    let Some(owner) = owner else { return Ok(()) };
    let paths = crate::firstboot::collaboration_identity_paths();
    let selected =
        tilo_user_identity::persist::read_device_cert(&paths.device_cert_for_user(root)).ok();
    native_identity::fence(&owner, room, selected.as_ref()).map_err(ActionError::Refused)?;
    if let Some(pending) = owner
        .pending_enrollment_for_slot(local)
        .map_err(ActionError::Refused)?
    {
        native_identity::fence(&owner, pending.id().room(), selected.as_ref())
            .map_err(ActionError::Refused)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use std::sync::Arc;

    #[test]
    fn topbar_editor_catalogue_opens_the_native_request_and_hides_in_fullscreen() {
        let chrome = chrome_with_room();
        let provider = ActionsProvider::new();
        let results = action_results(&provider, &chrome);
        let editor = results
            .iter()
            .find(|row| row.id == "topbar.editor.open")
            .expect("public editor action");
        assert_eq!(editor.operation_kind, tilo_actions::OperationKind::View);
        assert_eq!(editor.contract, Some(tilo_actions::ActionContract::view()));
        invoke_action(&results, "topbar.editor.open", &chrome);
        assert!(chrome.borrow_mut().take_topbar_editor_request());
        assert!(!chrome.borrow_mut().take_topbar_editor_request());
        {
            let mut state = chrome.borrow_mut();
            state.set_fullscreen_group(Some(1));
            state.set_fullscreen_presentation(true);
        }
        assert!(
            !action_results(&provider, &chrome)
                .iter()
                .any(|row| row.id == "topbar.editor.open")
        );
        assert!(!chrome.borrow_mut().take_topbar_editor_request());
    }

    fn chrome_rc() -> crate::shell::chrome_state::Chrome {
        let handle: WorkspaceTreeHandle =
            crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
        ChromeState::new_with_handle(handle)
    }

    /// A session with a room this machine administers — what `agent.invite` needs.
    fn chrome_with_room() -> crate::shell::chrome_state::Chrome {
        use crate::shell::state::{Workspace, WorkspaceId};
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut ws = Workspace::new(id, "Demo");
        // An explicit identity: the session registry lives under the user's
        // configuration directory, which is not this test's to read.
        ws.set_owner("shell-fixture");
        tree.desktop.workspaces.push(ws);
        tree.desktop.active = id;
        let handle: WorkspaceTreeHandle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        ChromeState::new_with_fixture_handle(
            handle,
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        )
    }

    fn action_results(
        provider: &ActionsProvider,
        chrome: &Chrome,
    ) -> Vec<crate::shell::search::SearchResult> {
        let document = AutoCommit::new();
        let state = chrome.borrow();
        provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &document,
            doc_root: &automerge::ROOT,
        })
    }

    fn invoke_action(results: &[crate::shell::search::SearchResult], id: &str, chrome: &Chrome) {
        let result = results
            .iter()
            .find(|result| result.id == id)
            .unwrap_or_else(|| panic!("action provider did not return {id}"));
        let mut document = AutoCommit::new();
        (result.on_open)(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            chrome,
            &ActionArgs::none(),
        )
        .unwrap();
    }

    #[test]
    fn published_display_keep_waits_for_durability_without_blocking_dispatch() {
        use settings_store::SettingsSource as _;
        use settings_store::hardware::{
            DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState,
        };
        use std::sync::mpsc;
        use std::time::Duration;
        let _home = tilo_test_env::TestHome::new().unwrap();
        let paths = settings_store::SessionPaths::configured().unwrap();
        let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
        settings_store::install_source(Some(session.host_view().unwrap()));
        let mut transaction =
            DisplayTransaction::request(DisplaySet::default(), DisplayConfig::default()).unwrap();
        transaction.state = DisplayTransactionState::AwaitingConfirmation;
        transaction.save().unwrap();
        let chrome = chrome_with_room();
        let results = action_results(&ActionsProvider::new(), &chrome);
        let action = results
            .iter()
            .find(|row| row.id == DISPLAY_KEEP_ID)
            .expect("published Keep action");
        let lock_path = paths.copy.with_file_name("settings.json.lock");
        let (dir, name) = tilo_files::file_at(&lock_path).unwrap();
        let lock =
            tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive).unwrap();
        let (sent, received) = mpsc::channel();
        let release = std::thread::spawn(move || {
            let responsive = received.recv_timeout(Duration::from_millis(500)).is_ok();
            drop(lock);
            responsive
        });
        let mut doc = AutoCommit::new();
        let mut edits = ui::widget::Edits::lend(&mut doc);
        let disposition = if let Some(begin) = &action.on_begin_invoke {
            begin(&mut edits, &automerge::ROOT, &chrome, &ActionArgs::none()).unwrap()
        } else {
            (action.on_open)(&mut edits, &automerge::ROOT, &chrome, &ActionArgs::none()).unwrap();
            crate::shell::search::InvocationDisposition::Accepted(
                crate::shell::search::ActionReceipt::completed(),
            )
        };
        if let crate::shell::search::InvocationDisposition::Pending(ticket) = &disposition {
            assert!(
                ticket().unwrap().is_none(),
                "held storage cannot already complete the job"
            );
        }
        let _ = sent.send(());
        let responsive = release.join().unwrap();
        settings_store::install_source(None);
        assert!(
            responsive,
            "published Keep dispatch blocked on the real storage lock"
        );
        let crate::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
            panic!("a storage command must retain its published job until durable completion");
        };
        crate::shell::durable_writer::global().flush();
        assert!(ticket().unwrap().is_some());
        let reopened = settings_store::SessionSettings::open(paths).unwrap();
        assert_eq!(reopened.load()["display_transaction"]["id"], transaction.id);
        assert_eq!(
            reopened.load()["display_transaction"]["state"],
            "confirm_requested"
        );
    }

    fn begin_display_answer(chrome: &Chrome, id: &str) -> crate::shell::search::PendingInvocation {
        let results = action_results(&ActionsProvider::new(), chrome);
        let row = results
            .iter()
            .find(|row| row.id == id)
            .expect("published answer");
        let mut doc = AutoCommit::new();
        let disposition = row.on_begin_invoke.as_ref().expect("answer lifecycle")(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            chrome,
            &ActionArgs::none(),
        )
        .unwrap();
        let crate::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
            panic!("storage must retain a real job");
        };
        ticket
    }

    #[test]
    fn published_display_answers_refuse_a_replacement_adopted_under_the_copy_lock() {
        use settings_store::SettingsSource as _;
        use settings_store::hardware::{
            DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState,
        };
        for id in [DISPLAY_KEEP_ID, DISPLAY_REVERT_ID] {
            let _home = tilo_test_env::TestHome::new().unwrap();
            let paths = settings_store::SessionPaths::configured().unwrap();
            let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
            settings_store::install_source(Some(session.host_view().unwrap()));
            let mut transaction =
                DisplayTransaction::request(DisplaySet::default(), DisplayConfig::default())
                    .unwrap();
            transaction.state = DisplayTransactionState::AwaitingConfirmation;
            transaction.save().unwrap();
            let lock_path = paths.copy.with_file_name("settings.json.lock");
            let (dir, name) = tilo_files::file_at(&lock_path).unwrap();
            let lock =
                tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive).unwrap();
            let ticket = begin_display_answer(&chrome_with_room(), id);
            assert!(ticket().unwrap().is_none());
            let mut replacement = transaction.clone();
            replacement.id += 100;
            replacement.state = DisplayTransactionState::Requested;
            let copy = serde_json::json!({"display_transaction": replacement});
            // The external writer owns the same canonical lock. Its copy is
            // adopted by SessionSettings before the conditional comparison.
            tilo_files::native::atomic::write_creating_parent(
                &paths.copy,
                &serde_json::to_vec(&copy).unwrap(),
            )
            .unwrap();
            drop(lock);
            crate::shell::durable_writer::global().flush();
            assert!(
                matches!(ticket(), Err(ActionError::Refused(message)) if message.contains("replaced"))
            );
            let reopened = settings_store::SessionSettings::open(paths).unwrap();
            assert_eq!(
                reopened.load()["display_transaction"],
                copy["display_transaction"]
            );
            settings_store::install_source(None);
        }
    }

    #[test]
    fn published_display_answers_report_real_record_failure_and_preserve_old_intent() {
        use settings_store::SettingsSource as _;
        use settings_store::hardware::{
            DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState,
        };
        for id in [DISPLAY_KEEP_ID, DISPLAY_REVERT_ID] {
            let _home = tilo_test_env::TestHome::new().unwrap();
            let paths = settings_store::SessionPaths::configured().unwrap();
            let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
            settings_store::install_source(Some(session.host_view().unwrap()));
            let mut transaction =
                DisplayTransaction::request(DisplaySet::default(), DisplayConfig::default())
                    .unwrap();
            transaction.state = DisplayTransactionState::AwaitingConfirmation;
            transaction.save().unwrap();
            std::fs::remove_file(&paths.device).unwrap();
            std::fs::create_dir(&paths.device).unwrap();
            let ticket = begin_display_answer(&chrome_with_room(), id);
            crate::shell::durable_writer::global().flush();
            assert!(matches!(ticket(), Err(ActionError::Refused(_))));
            assert_eq!(
                session.load()["display_transaction"],
                serde_json::to_value(&transaction).unwrap()
            );
            std::fs::remove_dir(&paths.device).unwrap();
            transaction.save().unwrap();
            let reopened = settings_store::SessionSettings::open(paths).unwrap();
            assert_eq!(
                reopened.load()["display_transaction"],
                serde_json::to_value(&transaction).unwrap()
            );
            settings_store::install_source(None);
        }
    }

    #[test]
    fn published_display_answer_requires_an_atomic_storage_receipt_even_if_state_matches() {
        use settings_store::SettingsSource as _;
        use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};
        use std::sync::mpsc;
        let _home = tilo_test_env::TestHome::new().unwrap();
        let transaction = DisplayTransaction {
            id: 77,
            state: DisplayTransactionState::AwaitingConfirmation,
            ..DisplayTransaction::default()
        };
        let document = serde_json::json!({"display_transaction": transaction})
            .as_object()
            .unwrap()
            .clone();
        let source = Arc::new(settings_store::HostDocument::new(document, |_| Ok(())));
        settings_store::install_source(Some(source.clone()));
        let (entered, started) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        crate::shell::durable_writer::global()
            .submit_ordered(move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
            })
            .unwrap();
        started.recv().unwrap();
        let ticket = begin_display_answer(&chrome_with_room(), DISPLAY_KEEP_ID);
        assert!(ticket().unwrap().is_none());
        // Another legitimate writer reaches the desired state. This cannot
        // turn an unsupported atomic operation into our storage receipt.
        source
            .update_section(
                "display_transaction",
                serde_json::json!({"state":"confirm_requested"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        release.send(()).unwrap();
        crate::shell::durable_writer::global().flush();
        assert!(
            matches!(ticket(), Err(ActionError::Refused(message)) if message.contains("conditional settings persistence is unavailable"))
        );
        assert_eq!(
            source.load()["display_transaction"]["state"],
            "confirm_requested"
        );
        settings_store::install_source(None);
    }

    #[test]
    fn published_display_answers_accept_real_durable_record_with_copy_repair_pending() {
        use settings_store::SettingsSource as _;
        use settings_store::hardware::{
            DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState,
        };
        for id in [DISPLAY_KEEP_ID, DISPLAY_REVERT_ID] {
            let _home = tilo_test_env::TestHome::new().unwrap();
            let paths = settings_store::SessionPaths::configured().unwrap();
            let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
            settings_store::install_source(Some(session.host_view().unwrap()));
            let mut transaction =
                DisplayTransaction::request(DisplaySet::default(), DisplayConfig::default())
                    .unwrap();
            transaction.state = DisplayTransactionState::AwaitingConfirmation;
            transaction.save().unwrap();
            std::fs::remove_file(&paths.copy).unwrap();
            std::fs::create_dir(&paths.copy).unwrap();
            let ticket = begin_display_answer(&chrome_with_room(), id);
            crate::shell::durable_writer::global().flush();
            assert!(ticket().unwrap().is_some());
            let state = if id == DISPLAY_KEEP_ID {
                "confirm_requested"
            } else {
                "revert_requested"
            };
            assert_eq!(session.load()["display_transaction"]["state"], state);
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&paths.device).unwrap()).unwrap();
            assert_eq!(record["display_transaction"]["id"], transaction.id);
            assert_eq!(record["display_transaction"]["state"], state);
            let accepted_revision = session.revision();
            std::fs::remove_dir(&paths.copy).unwrap();
            session.refresh();
            assert_eq!(
                session.revision(),
                accepted_revision,
                "copy repair must not republish unchanged committed state"
            );
            let repaired: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&paths.copy).unwrap()).unwrap();
            assert_eq!(
                repaired["display_transaction"],
                record["display_transaction"]
            );
            settings_store::install_source(None);
        }
    }

    #[test]
    fn axis_navigation_action_door_is_session_local_and_opt_in() {
        let chrome = chrome_with_room();
        let results = action_results(&ActionsProvider::new(), &chrome);
        assert!(!chrome.borrow().axis_navigation_enabled());
        let gather = results
            .iter()
            .find(|row| row.id == "navigation.axis.gather")
            .unwrap();
        let mut document = AutoCommit::new();
        assert!(
            (gather.on_open)(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none()
            )
            .is_err()
        );
        invoke_action(&results, "navigation.axis.enable", &chrome);
        assert!(chrome.borrow().axis_navigation_enabled());
        invoke_action(&results, "navigation.axis.gather", &chrome);
        assert!(chrome.borrow_mut().take_axis_gather_request());
        let other = chrome_with_room();
        assert!(!other.borrow().axis_navigation_enabled());
        invoke_action(&results, "navigation.axis.disable", &chrome);
        assert!(!chrome.borrow().axis_navigation_enabled());
        assert!(!chrome.borrow_mut().take_axis_gather_request());
    }

    fn begin_lifecycle_action(
        results: &[crate::shell::search::SearchResult],
        id: &str,
        chrome: &Chrome,
    ) -> crate::shell::search::InvocationDisposition {
        let result = results
            .iter()
            .find(|result| result.id == id)
            .unwrap_or_else(|| panic!("action provider did not return {id}"));
        let begin = result
            .on_begin_invoke
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must use lifecycle dispatch"));
        let mut document = AutoCommit::new();
        begin(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            chrome,
            &ActionArgs::none(),
        )
        .unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn shell_actions_dispatch_through_the_provider_and_report_real_results() {
        // Phase `panel_actions_share_the_provider_dispatch_path`.
        {
            let provider = ActionsProvider::new();
            let chrome = chrome_with_room();
            let results = action_results(&provider, &chrome);

            invoke_action(&results, "settings.open", &chrome);
            assert!(chrome.borrow().overlays.ws_settings);

            invoke_action(&results, "quick-settings.open", &chrome);
            {
                let state = chrome.borrow();
                assert!(state.overlays.quick_settings);
                assert!(!state.overlays.ws_settings);
                assert_eq!(
                    state.quick_settings.current_level(),
                    &crate::shell::quick_settings::QsLevel::Root
                );
            }

            invoke_action(&results, "quick-settings.clipboard", &chrome);
            assert_eq!(
                chrome.borrow().quick_settings.current_level(),
                &crate::shell::quick_settings::QsLevel::Clipboard
            );

            invoke_action(&results, "quick-settings.devices", &chrome);
            assert_eq!(
                chrome.borrow().quick_settings.current_level(),
                &crate::shell::quick_settings::QsLevel::Devices
            );
        }

        // Phase `collaboration_actions_enqueue_typed_requests_through_the_provider`.
        {
            let provider = ActionsProvider::new();
            let chrome = chrome_with_room();
            chrome.borrow_mut().peer_sync_enabled = true;
            let results = action_results(&provider, &chrome);

            for id in ["peer.invite", "peer.connect"] {
                assert!(matches!(
                    begin_lifecycle_action(&results, id, &chrome),
                    crate::shell::search::InvocationDisposition::Pending(_)
                ));
            }

            let queued = chrome.borrow_mut().take_peer_sync_requests();
            assert_eq!(queued.len(), 2);
            let actions = queued
                .into_iter()
                .map(|request| match request {
                    crate::shell::overlay_host::PeerSyncAction::Tracked { action, .. } => *action,
                    other => panic!("lifecycle request must remain tracked, got {other:?}"),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                actions,
                vec![
                    crate::shell::overlay_host::PeerSyncAction::PromptInvite,
                    crate::shell::overlay_host::PeerSyncAction::PromptConnect,
                ]
            );
        }

        // Phase `room_activity_action_opens_the_sheet_on_open`.
        {
            let p = ActionsProvider::new();
            let c = chrome_with_room();
            let mut doc = AutoCommit::new();
            let out = {
                let cs = c.borrow();
                p.query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "activity",
                    wanted: &[ResourceKind::Action],
                    chrome: &cs,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
            };
            let action = out
                .iter()
                .find(|r| r.id == "room.activity.open")
                .expect("typing 'activity' must surface the room-activity action");
            assert_eq!(action.title, "Show workspace activity");
            assert!(!c.borrow().overlays.room_activity, "starts closed");
            (action.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &c,
                &ActionArgs::none(),
            )
            .expect("a nullary chrome action must run");
            assert!(
                c.borrow().overlays.room_activity,
                "invoking the exported action opened the sheet"
            );
            assert!(
                !c.borrow().overlays.collaborators,
                "the dropdown the sheet can also be opened from is dismissed with it"
            );
        }

        // Phase `dnd_action_toggles_flag_on_open`.
        {
            let p = ActionsProvider::new();
            let c = chrome_with_room();
            let mut doc = AutoCommit::new();
            let out = {
                let cs = c.borrow();
                p.query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "do not disturb",
                    wanted: &[ResourceKind::Action],
                    chrome: &cs,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
            };
            let dnd = out
                .iter()
                .find(|r| r.id == "notifications.dnd.toggle")
                .expect("DND action must be present");
            assert!(!c.borrow().dnd(), "DND starts off");
            let fire = |doc: &mut ui::widget::Edits<'_>| {
                (dnd.on_open)(doc, &automerge::ROOT, &c, &ActionArgs::none())
                    .expect("a nullary chrome action must run");
            };
            fire(&mut ui::widget::Edits::lend(&mut doc));
            assert!(c.borrow().dnd(), "opening the action turns DND on");
            fire(&mut ui::widget::Edits::lend(&mut doc));
            assert!(!c.borrow().dnd(), "opening it again turns DND back off");
        }

        // Phase `tab_duplicate_failure_reaches_the_invoker_and_keeps_user_feedback`.
        {
            let action = Action::ALL
                .iter()
                .find(|action| action.id == "tab.duplicate")
                .expect("duplicate action is registered");
            let chrome =
                ChromeState::new_with_handle(crate::shell::state::WorkspaceTreeHandle::new(
                    crate::shell::state::WorkspaceTree::default(),
                ));
            let args = ActionArgs::bind(action.params, []).expect("duplicate is nullary");
            let error = (action.apply)(&mut chrome.borrow_mut(), &args)
                .expect_err("no active tab must be a machine-readable failure");
            assert!(
                matches!(error, ActionError::Refused(message) if message.contains("no tab") || message.contains("No tab"))
            );
            assert!(
                !chrome.borrow().notifications().is_empty(),
                "the person still receives useful visible feedback"
            );
        }

        // Phase `record_bring_reports_the_actual_queued_result`.
        {
            let provider = ActionsProvider::new();
            let chrome = chrome_with_room();
            let mut doc = AutoCommit::new();
            let result = {
                let state = chrome.borrow();
                provider
                    .query(SearchContext {
                        query_scope: None,
                        include_programmatic: false,
                        needle: "record.bring",
                        wanted: &[ResourceKind::Action],
                        chrome: &state,
                        doc: &doc,
                        doc_root: &automerge::ROOT,
                    })
                    .into_iter()
                    .find(|result| result.id == "record.bring")
                    .expect("record action")
            };
            let args = ActionArgs::bind(
                &result.params,
                [(
                    "id".into(),
                    crate::shell::search::action_api::ArgValue::Text("file:/tmp/example".into()),
                )],
            )
            .expect("record arguments");
            let disposition = result
                .on_begin_invoke
                .as_ref()
                .expect("record bring has lifecycle admission")(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                &args,
            )
            .expect("request is admitted to the room index");
            let crate::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
                panic!("queued record work must not report terminal success");
            };
            assert!(ticket().expect("pending poll").is_none());
            chrome
                .borrow_mut()
                .complete_record_request(1, Err("controlled index refusal".into()));
            assert!(
                matches!(ticket(), Err(ActionError::Refused(message)) if message.contains("controlled"))
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn shell_action_matching_ranks_prefix_substring_subtitle_and_fuzzy() {
        // Phase `prefix_beats_substring_beats_subtitle`.
        {
            let p = ActionsProvider::new();
            let c = chrome_rc();
            let cs = c.borrow();
            let doc = AutoCommit::new();
            let out = p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "open",
                wanted: &[ResourceKind::Action],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            let opens: Vec<_> = out.iter().filter(|r| r.title.starts_with("Open")).collect();
            assert!(opens.iter().all(|r| r.score == 100));
        }

        // Phase `action_ids_and_out_of_order_terms_are_searchable`.
        {
            let p = ActionsProvider::new();
            let c = chrome_rc();
            let cs = c.borrow();
            let doc = AutoCommit::new();
            let score = |needle: &str| {
                p.query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle,
                    wanted: &[ResourceKind::Action],
                    chrome: &cs,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|row| row.id == "settings.privacy.open")
                .map(|row| row.score)
            };
            assert_eq!(score("settings.privacy.open"), Some(100));
            assert_eq!(
                score("security workspace"),
                Some(crate::shell::search::provider_score(
                    ui::search::MatchTier::Spanning.percent()
                )),
            );
        }

        // Phase `empty_needle_returns_everything`.
        {
            let p = ActionsProvider::new();
            // Peer-sync and agent rows included, so "everything" means the whole table.
            let c = chrome_with_room();
            c.borrow_mut().peer_sync_enabled = true;
            let cs = c.borrow();
            let doc = AutoCommit::new();
            let out = p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[ResourceKind::Action],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert!(
                Action::ALL
                    .iter()
                    .all(|action| out.iter().any(|result| result.id == action.id))
            );
            assert!(out.iter().all(|r| r.score == 0));
        }

        // Phase `unrequested_kind_returns_empty`.
        {
            let p = ActionsProvider::new();
            let c = chrome_rc();
            let cs = c.borrow();
            let doc = AutoCommit::new();
            let out = p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "open",
                wanted: &[ResourceKind::File],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert!(out.is_empty());
        }

        // Phase `a_typo_still_matches`.
        {
            let p = ActionsProvider::new();
            let c = chrome_rc();
            let cs = c.borrow();
            let doc = AutoCommit::new();
            let out = p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "workspace setings",
                wanted: &[ResourceKind::Action],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            assert!(out.iter().any(|r| r.id == "settings.open"));
        }
    }

    #[test]
    fn typing_open_on_a_german_desktop_does_not_prefer_the_tour() {
        crate::shell::i18n::ensure();
        ui::i18n::I18nManager::set_locale("de-DE");
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "open",
            wanted: &[ResourceKind::Action],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        ui::i18n::I18nManager::set_locale("en-US");
        assert!(
            !out.is_empty(),
            "an English 'open' still finds the *.open addresses"
        );
        assert_ne!(
            out[0].id,
            "desktop-tour.open",
            "equal id-scores must not put the tour first; got {:?}",
            out.iter()
                .take(5)
                .map(|r| (&r.id, r.score))
                .collect::<Vec<_>>()
        );
        let tour = out.iter().position(|r| r.id == "desktop-tour.open");
        let settings = out.iter().position(|r| r.id == "settings.open");
        assert!(
            settings.is_some() && tour.is_some() && settings < tour,
            "settings.open ({settings:?}) should outrank the tour ({tour:?})"
        );
    }

    /// Every offered action resolves by its address, however many are
    /// offered: the table is longer than a palette page.
    #[test]
    fn every_offered_action_resolves_by_its_address() {
        let p = ActionsProvider::new();
        let doc = AutoCommit::new();
        let c = chrome_with_room();
        c.borrow_mut().peer_sync_enabled = true;
        let chrome = c.borrow();
        let ctx = || SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &chrome,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let offered: Vec<String> = p.query(ctx()).into_iter().map(|result| result.id).collect();
        assert!(
            offered.len() > crate::shell::search::SearchBudget::PALETTE.max_results,
            "the table is longer than a page, or this checks nothing: {}",
            offered.len()
        );
        for id in &offered {
            assert!(
                p.resolve(id, ctx()).is_some_and(|result| &result.id == id),
                "{id} is offered but does not resolve"
            );
        }
    }

    /// The rows `provider` offers for `needle` against `chrome` as it is now.
    fn rows_for(
        provider: &ActionsProvider,
        chrome: &Chrome,
        needle: &str,
    ) -> Vec<crate::shell::search::SearchResult> {
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
    }

    fn row<'a>(
        rows: &'a [crate::shell::search::SearchResult],
        id: &str,
    ) -> Option<&'a crate::shell::search::SearchResult> {
        rows.iter().find(|row| row.id == id)
    }

    #[test]
    fn empty_palette_builds_the_catalogue_once() {
        let provider = Rc::new(ActionsProvider::new());
        let builds = Rc::clone(&provider.builds);
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(provider);
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let grouped = |needle: &str| {
            registry.query_grouped(
                &crate::shell::search::QueryArgs {
                    needle,
                    scope: crate::shell::search::SearchScope::All,
                    active_provider_id: None,
                    wanted_kinds: &[],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                },
                // The palette's row budget.
                8,
            )
        };

        let empty = grouped("");
        let suggestions = empty
            .iter()
            .filter(|group| group.kind == ResourceKind::Action)
            .flat_map(|group| group.results.iter().map(|result| result.id.as_str()))
            .collect::<Vec<_>>();
        assert!(
            suggestions.contains(&"room.activity.open")
                && suggestions.contains(&"shortcut-help.toggle"),
            "the empty palette looks its suggestions up: {suggestions:?}"
        );
        assert_eq!(
            builds.get(),
            1,
            "one fan-out query and its suggestion lookups share one build"
        );
        grouped("");
        assert_eq!(builds.get(), 1, "nothing changed, so nothing is rebuilt");
        for needle in ["d", "da", "dar"] {
            assert!(!grouped(needle).is_empty(), "{needle:?} finds actions");
        }
        let found = grouped("dark")
            .into_iter()
            .flat_map(|group| group.results)
            .any(|result| result.id == "appearance.theme.dark");
        assert!(found, "\"dark\" finds the dark theme");
        assert_eq!(builds.get(), 1, "typing narrows the retained rows");
    }

    #[test]
    fn resolve_does_not_rank() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let ctx = |needle| SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let tour = provider
            .resolve("desktop-tour.open", ctx(""))
            .expect("the tour resolves by its address");
        assert_eq!(tour.id, "desktop-tour.open");
        assert_eq!(tour.title, t("search-action-desktop-tour-open"));
        assert_eq!(provider.builds.get(), 1);
        assert!(
            provider.resolve("shortcut-help.toggle", ctx("")).is_some()
                && provider.resolve("no.such.action", ctx("")).is_none()
        );
        assert!(
            provider
                .resolve("desktop-tour.open", ctx("something else entirely"))
                .is_some(),
            "an address is not a search"
        );
        assert_eq!(
            provider
                .query(ctx("tour"))
                .first()
                .map(|row| row.id.as_str()),
            Some("desktop-tour.open")
        );
        assert_eq!(
            provider.builds.get(),
            1,
            "lookups and queries in one state share one build"
        );
        assert!(
            provider
                .resolve(
                    "desktop-tour.open",
                    SearchContext {
                        wanted: &[ResourceKind::File],
                        ..ctx("")
                    }
                )
                .is_none(),
            "a lookup that does not want actions finds none"
        );
    }

    #[test]
    fn catalogue_rebuilds_on_locale_and_workspace_change() {
        struct RestoreLocale;
        impl Drop for RestoreLocale {
            fn drop(&mut self) {
                ui::i18n::I18nManager::set_locale("en-US");
            }
        }
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let language = |provider: &ActionsProvider| {
            row(&rows_for(provider, &chrome, ""), "appearance.language")
                .map(|row| row.title.clone())
        };

        // Locale.
        assert_eq!(language(&provider).as_deref(), Some("Set language"));
        let _restore = RestoreLocale;
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(crate::shell::i18n::current_locale(), "de-DE");
        assert_eq!(language(&provider).as_deref(), Some("Sprache festlegen"));
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(language(&provider).as_deref(), Some("Set language"));

        // Active workspace: governance rows address the workspace now active.
        let (first_room, second, second_room) = {
            use crate::shell::state::{Workspace, WorkspaceId};
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let first_room = tree.desktop.active_workspace().expect("fixture").room;
            let id = WorkspaceId::new();
            let mut workspace = Workspace::new(id, "Second");
            workspace.set_owner("shell-fixture");
            let room = workspace.room;
            tree.desktop.workspaces.push(workspace);
            drop(tree);
            (first_room, id, room)
        };
        assert_ne!(first_room, second_room);
        let proposal = |provider: &ActionsProvider| {
            rows_for(provider, &chrome, "ownership")
                .into_iter()
                .find(|row| row.id.ends_with("#ownership.propose"))
                .map(|row| row.id)
        };
        assert_eq!(
            proposal(&provider),
            Some(format!("workspace:{first_room}#ownership.propose"))
        );
        chrome.borrow().tree_mut().desktop.active = second;
        assert_eq!(
            proposal(&provider),
            Some(format!("workspace:{second_room}#ownership.propose")),
            "switching workspace rebuilds its governance rows"
        );
    }

    #[test]
    fn catalogue_rebuilds_on_tree_mutations_and_per_desktop() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let (workspace_id, workspace_room) = {
            let state = chrome.borrow();
            let tree = state.tree();
            let workspace = tree.desktop.active_workspace().expect("fixture");
            let located = (workspace.id, workspace.room);
            drop(tree);
            located
        };

        // A tree mutation the chrome made, which it bumps the revision for.
        let switch_to = |provider: &ActionsProvider| {
            row(
                &rows_for(provider, &chrome, ""),
                &format!("workspace.select.{}", workspace_id.0),
            )
            .map(|row| row.title.clone())
            .expect("every workspace can be switched to")
        };
        assert!(switch_to(&provider).contains("Demo"));
        {
            let mut state = chrome.borrow_mut();
            state
                .tree_mut()
                .desktop
                .active_workspace_mut()
                .expect("workspace")
                .name = "Renamed".into();
            state.bump();
        }
        assert!(switch_to(&provider).contains("Renamed"));

        // A mutation from outside the chrome, before a tick folds it in.
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            tree.desktop.active_workspace_mut().expect("workspace").name = "Remote".into();
            tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        }
        assert!(switch_to(&provider).contains("Remote"));

        // Another desktop that differs only in being another: the same
        // workspace id and revisions, its own room.
        let other = {
            use crate::shell::state::Workspace;
            let mut tree = WorkspaceTree::default();
            let mut workspace = Workspace::new(workspace_id, "Remote");
            workspace.set_owner("shell-fixture");
            tree.desktop.workspaces.push(workspace);
            tree.desktop.active = workspace_id;
            tree.desktop.external_revision = chrome.borrow().tree().desktop.external_revision;
            ChromeState::new_with_fixture_handle(
                crate::shell::state::WorkspaceTreeHandle::new(tree),
                crate::shell::hosted_inputs::HostedTileInputs::default(),
            )
        };
        other.borrow_mut().revision = chrome.borrow().revision;
        let other_room = other
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("fixture")
            .room;
        assert_ne!(other_room, workspace_room);
        let other_proposal = rows_for(&provider, &other, "ownership")
            .into_iter()
            .find(|row| row.id.ends_with("#ownership.propose"))
            .map(|row| row.id);
        assert_eq!(
            other_proposal,
            Some(format!("workspace:{other_room}#ownership.propose")),
            "one provider asked about two desktops answers for each"
        );
    }

    /// The palette offers the invite rows exactly where the one invitation
    /// policy lets this device issue, as the invite composer is: not in a
    /// room another person governs, where the issuer would refuse.
    #[test]
    fn invite_rows_follow_the_invitation_policy() {
        use crate::shell::state::{PeerOrigin, RoomAuthority};

        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        chrome.borrow_mut().peer_sync_enabled = true;
        let offers = |id: &str| row(&rows_for(&provider, &chrome, ""), id).is_some();
        let route = || crate::shell::invite_policy::active_invite_route(&chrome.borrow());
        assert_eq!(
            route(),
            Some(crate::shell::invite_policy::InviteRoute::ShareOnRelay),
            "the owner of a room this device governs may invite"
        );
        assert!(offers("peer.invite"), "so the palette offers the composer");

        let room = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("workspace")
            .room;
        let handle = chrome.borrow().tree_handle();
        handle.write().room_authority.insert(
            room,
            RoomAuthority::Peer(PeerOrigin::peer(&iroh::SecretKey::generate().public())),
        );
        assert_eq!(
            route(),
            Some(crate::shell::invite_policy::InviteRoute::ViaGovernor),
            "another device governs the room now"
        );
        assert!(!offers("peer.invite"), "so no invite row is offered");
        assert!(!offers("peer.invite.named"), "named or not");
        assert!(offers("peer.connect"), "joining a room is not inviting");
    }

    #[test]
    fn catalogue_rebuilds_on_collaboration_and_search_sources() {
        // Peer sync and the collaboration broker.
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let offers = |provider: &ActionsProvider, id: &str| {
            row(&rows_for(provider, &chrome, ""), id).is_some()
        };
        assert!(!offers(&provider, "peer.invite"));
        chrome.borrow_mut().peer_sync_enabled = true;
        assert!(offers(&provider, "peer.invite"), "peer sync started");
        assert!(offers(&provider, "agent.invite"));
        chrome
            .borrow_mut()
            .set_collab_unavailable("the broker could not start");
        assert!(!offers(&provider, "agent.invite"), "the broker went away");

        // Search sources: one registered, and one turned off.
        let mut registry = crate::shell::search::SearchRegistry::new();
        let provider = ActionsProvider::with_preferences_and_catalog(
            registry.preferences_handle(),
            registry.provider_catalog_handle(),
        );
        let chrome = chrome_rc();
        let switch = |provider: &ActionsProvider, id: &str| {
            row(
                &rows_for(provider, &chrome, ""),
                &format!("search.provider.toggle:{id}"),
            )
            .map(|row| row.title.clone())
        };
        let windows = super::super::desktop_windows::DesktopWindowsProvider::new();
        let windows_id = windows.id().to_string();
        let windows_name = windows.display_name();
        assert_eq!(switch(&provider, &windows_id), None);
        registry.register(Rc::new(windows));
        let source: &[(&str, ui::i18n::ArgValue)] = &[
            ("source", windows_name.into()),
            ("id", windows_id.clone().into()),
        ];
        assert_eq!(
            switch(&provider, &windows_id),
            Some(t_args("search-action-provider-disable", source)),
            "a newly registered source gets its switch"
        );
        registry.set_provider_enabled(&windows_id, false);
        assert_eq!(
            switch(&provider, &windows_id),
            Some(t_args("search-action-provider-enable", source)),
            "a source turned off is offered to turn back on"
        );
    }

    #[test]
    fn catalogue_rebuilds_on_quick_settings_backend_and_availability() {
        // The Quick Settings backend, what it can do, and a swap of either.
        let first = Rc::new(RecordingQsActions::default());
        let slot = Rc::new(SharedQsDispatcher::new(
            first.clone(),
            crate::shell::quick_settings::actions::QsActionAvailability::all(),
        ));
        let provider = ActionsProvider::with_shared_qs_actions(Rc::clone(&slot));
        let chrome = chrome_with_room();
        let set_volume = |provider: &ActionsProvider| {
            let rows = rows_for(provider, &chrome, "");
            let volume = row(&rows, QS_VOLUME_ID).expect("volume action");
            let args = ActionArgs::bind(
                &volume.params,
                [(
                    "percent".into(),
                    crate::shell::search::action_api::ArgValue::Number(40.0),
                )],
            )
            .expect("volume arguments");
            let mut doc = AutoCommit::new();
            (volume.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                &args,
            )
        };
        set_volume(&provider).expect("the first backend sets the volume");
        assert_eq!(*first.calls.borrow(), ["volume:40"]);
        let second = Rc::new(RecordingQsActions::default());
        slot.replace(
            second.clone(),
            crate::shell::quick_settings::actions::QsActionAvailability::all(),
        );
        set_volume(&provider).expect("the second backend sets the volume");
        assert_eq!(
            *first.calls.borrow(),
            ["volume:40"],
            "a replaced backend is not called"
        );
        assert_eq!(*second.calls.borrow(), ["volume:40"]);
        let volume_available = |provider: &ActionsProvider| {
            row(&rows_for(provider, &chrome, ""), QS_VOLUME_ID)
                .and_then(|row| row.contract.as_ref())
                .expect("volume contract")
                .availability
                .is_available()
        };
        assert!(volume_available(&provider));
        slot.replace(
            second,
            crate::shell::quick_settings::actions::QsActionAvailability {
                volume: false,
                ..crate::shell::quick_settings::actions::QsActionAvailability::all()
            },
        );
        assert!(!volume_available(&provider), "the volume control went away");
    }

    #[test]
    fn workspace_activity_exact_local_actions_are_exported() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let out = provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "activity",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        for id in [
            "room.activity.open",
            "room.activity.explore",
            "room.activity.replay",
            "room.activity.acknowledge",
            "room.activity.tasks",
        ] {
            assert!(
                out.iter().any(|action| action.id == id
                    && action.operation_kind == tilo_actions::OperationKind::View),
                "{id}"
            );
        }
    }

    #[test]
    fn undo_and_restore_name_the_selected_row_actions() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let query = |needle| {
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        for (needle, id) in [
            ("undo", tilo_desktop::activity::HISTORY_UNDO),
            ("restore", tilo_desktop::activity::HISTORY_RESTORE),
            ("compare", tilo_desktop::activity::HISTORY_COMPARE),
        ] {
            assert!(
                query(needle).iter().any(|row| row.id == id),
                "{needle} must surface {id}"
            );
        }
    }

    #[test]
    fn tab_menu_is_searchable_as_a_view_and_refuses_empty_focus() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let rows = {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "tab menu",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let menu = rows
            .iter()
            .find(|row| row.id == "tab.menu")
            .expect("search discovers the menu");
        assert_eq!(menu.operation_kind, tilo_actions::OperationKind::View);
        let mut doc = AutoCommit::new();
        let result = (menu.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        );
        assert!(matches!(result, Err(ActionError::Refused(_))));
        assert!(
            chrome
                .borrow_mut()
                .take_shortcut_action_requests()
                .is_empty()
        );
    }

    #[test]
    fn history_finds_activity_views_and_not_a_workspace_switch() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let query = |needle| {
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };

        let history = query("history");
        let explore = history
            .iter()
            .find(|row| row.id == "room.activity.explore")
            .expect("history names the full timeline");
        assert_eq!(
            explore.subtitle.as_deref(),
            Some("Every change here, by person, app and time")
        );
        assert!(
            history
                .iter()
                .all(|row| !row.id.starts_with("workspace.select.")),
            "switching workspace is not an answer to “history”: {:?}",
            history.iter().map(|row| &row.title).collect::<Vec<_>>()
        );

        let switch = query("switch");
        let row = switch
            .iter()
            .find(|row| row.id.starts_with("workspace.select."))
            .expect("a workspace is still reachable by switching to it");
        assert_eq!(
            row.subtitle.as_deref(),
            Some("Go to this workspace and its windows")
        );
    }

    #[test]
    fn a_search_for_a_source_finds_it_not_its_off_switch() {
        let preferences = Rc::new(std::cell::RefCell::new(
            crate::shell::search::SearchPreferences::default(),
        ));
        let catalog = Rc::new(std::cell::RefCell::new(vec![(
            "notes".to_string(),
            "Notes".to_string(),
        )]));
        let provider = ActionsProvider::with_preferences_and_catalog(preferences, catalog);
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let switch = |needle| {
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle,
                    wanted: &[ResourceKind::Action],
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|row| row.id == "search.provider.toggle:notes")
        };

        assert!(switch("notes").is_none(), "“notes” looks for notes");
        let row = switch("disable notes").expect("asking to disable it finds the switch");
        assert_eq!(
            row.subtitle.as_deref(),
            Some("Hide Notes results from search and agent discovery")
        );
        assert!(
            switch("").is_some(),
            "a complete listing keeps every switch"
        );
        assert!(
            switch("search.provider.toggle:notes").is_some(),
            "an exact address still resolves"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn typed_shell_actions_deep_link_to_their_exact_surface() {
        // Phase `search_source_customization_is_a_typed_exported_action`.
        {
            let preferences = Rc::new(std::cell::RefCell::new(
                crate::shell::search::SearchPreferences::default(),
            ));
            let catalog = Rc::new(std::cell::RefCell::new(vec![
                ("actions".to_string(), "Actions".to_string()),
                ("files".to_string(), "Files".to_string()),
            ]));
            let provider =
                ActionsProvider::with_preferences_and_catalog(Rc::clone(&preferences), catalog);
            let chrome = chrome_rc();
            let doc = AutoCommit::new();
            let results = provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "search source",
                wanted: &[ResourceKind::Action],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            let action = results
                .iter()
                .find(|result| result.id == "search.provider.set")
                .expect("search customization must be reachable without clicking settings UI");
            assert_eq!(action.params, SEARCH_PROVIDER_SET_PARAMS);
            assert_eq!(action.params[0].ty, ParamType::Text);
            assert_eq!(action.params[1].ty, ParamType::Boolean);
            let direct = results
                .iter()
                .find(|result| result.id == "search.provider.toggle:files")
                .expect("people can customize a named source without knowing its opaque id");
            assert_eq!(direct.title, "Disable Files search");
            assert_eq!(direct.badge.as_deref(), Some("ON"));
            assert!(direct.params.is_empty());
            assert!(
                results
                    .iter()
                    .all(|result| result.id != "search.provider.toggle:actions"),
                "the accessibility and automation source cannot offer a disable affordance"
            );

            preferences
                .borrow_mut()
                .set_provider_enabled("files", false);
            let disabled = provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "files search",
                wanted: &[ResourceKind::Action],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            let direct = disabled
                .iter()
                .find(|result| result.id == "search.provider.toggle:files")
                .expect("a disabled source remains directly re-enableable");
            assert_eq!(direct.title, "Enable Files search");
            assert_eq!(direct.badge.as_deref(), Some("OFF"));
        }

        // Phase `workspace_privacy_search_result_deep_links_to_exact_lens_tab`.
        {
            let p = ActionsProvider::new();
            let c = chrome_with_room();
            let mut doc = AutoCommit::new();
            let out = {
                let state = c.borrow();
                p.query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "workspace privacy",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
            };
            let result = out
                .iter()
                .find(|result| result.id == "settings.privacy.open")
                .expect("privacy governance is directly searchable");

            (result.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &c,
                &ActionArgs::none(),
            )
            .unwrap();

            let state = c.borrow();
            assert!(state.overlays.ws_settings);
            assert_eq!(
                state.ws_settings_tab,
                tilo_desktop::workspace_settings::WsTab::Permissions
            );
        }

        // Phase `status_privacy_is_a_typed_semantic_action`.
        {
            let provider = ActionsProvider::new();
            let chrome = chrome_with_room();
            let mut doc = AutoCommit::new();
            let results = {
                let state = chrome.borrow();
                provider.query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "status privacy",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
            };
            let action = results
                .iter()
                .find(|result| result.id == "presence.status-sharing.set")
                .expect("agents must be able to control the same status privacy setting");
            let args = ActionArgs::bind(
                &action.params,
                [(
                    "mode".to_string(),
                    tilo_actions::ArgValue::Text("off".into()),
                )],
            )
            .unwrap();

            (action.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                &args,
            )
            .unwrap();

            assert_eq!(
                chrome.borrow_mut().take_presence_requests(),
                vec![
                    crate::shell::chrome_state::PresenceRequest::SetStatusSharing(
                        ui::StatusSharing::Off
                    )
                ]
            );
        }

        // Phase `desktop_tour_is_searchable_and_requests_the_trusted_chrome_modal`.
        {
            let provider = ActionsProvider::new();
            let chrome = chrome_with_room();
            let mut doc = AutoCommit::new();
            let results = {
                let state = chrome.borrow();
                provider.query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "desktop tour",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
            };
            let result = results
                .iter()
                .find(|result| result.id == "desktop-tour.open")
                .expect("the completed first-use tour must remain discoverable");

            (result.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none(),
            )
            .unwrap();
            assert!(chrome.borrow().overlays.desktop_tour);
        }

        // Phase `shortcut_help_search_result_teaches_both_help_routes`.
        {
            let action = Action::ALL
                .iter()
                .find(|action| action.id == "shortcut-help.toggle")
                .expect("shortcut help action");
            assert_eq!(t(action.label), "Show desktop help");
            let subtitle = t(action.subtitle);
            assert!(subtitle.contains("Super+/"));
            assert!(subtitle.contains("F1"));
            assert!(subtitle.contains("same guide"));
        }
    }

    /// Row titles and subtitles are catalogue keys, which the source scan
    /// cannot see through the table; every one must resolve in every locale.
    #[test]
    fn every_action_row_resolves_to_real_copy() {
        crate::shell::i18n::ensure();
        for (locale, title, subtitle) in [
            (
                "en-US",
                "Edit top bar",
                "Reorder, add or remove top-bar widgets",
            ),
            (
                "de-DE",
                "Obere Leiste bearbeiten",
                "Widgets der oberen Leiste neu anordnen, hinzufügen oder entfernen",
            ),
        ] {
            ui::i18n::I18nManager::set_locale(locale);
            let editor = Action::ALL
                .iter()
                .find(|action| action.id == "topbar.editor.open")
                .expect("top-bar editor action");
            assert_eq!(t(editor.label), title);
            assert_eq!(t(editor.subtitle), subtitle);
            for action in Action::ALL {
                for key in [action.label, action.subtitle] {
                    let copy = t(key);
                    assert_ne!(copy, key, "{locale}: {key} has no entry in the catalogue");
                    assert!(
                        !copy.trim().is_empty(),
                        "{locale}: {key} resolves to nothing"
                    );
                }
            }
        }
        ui::i18n::I18nManager::set_locale("en-US");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn theme_and_language_actions_are_searchable_and_write_the_settings_store() {
        let config = tempfile::tempdir().expect("isolated settings");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());

        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let query = |needle: &str| {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let id_of = |needle: &str, want: &str| {
            query(needle)
                .into_iter()
                .find(|row| row.id == want)
                .unwrap_or_else(|| panic!("{needle:?} must find {want}"))
                .id
        };
        assert_eq!(
            id_of("dark theme", "appearance.theme.dark"),
            "appearance.theme.dark"
        );
        assert_eq!(
            id_of("light theme", "appearance.theme.light"),
            "appearance.theme.light"
        );
        assert_eq!(
            id_of("high contrast", "appearance.high-contrast"),
            "appearance.high-contrast"
        );
        assert_eq!(
            id_of("language", "appearance.language"),
            "appearance.language"
        );

        invoke_action(&query("dark theme"), "appearance.theme.dark", &chrome);
        assert!(
            chrome.borrow().appearance_generation > 0,
            "theme actions must wake the live frame, not only write the store"
        );
        let appearance = settings_store::load_section("appearance");
        assert_eq!(
            appearance.get("mode").and_then(serde_json::Value::as_str),
            Some("dark")
        );
        assert_eq!(
            appearance
                .get("high_contrast")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );

        invoke_action(&query("high contrast"), "appearance.high-contrast", &chrome);
        let appearance = settings_store::load_section("appearance");
        assert_eq!(
            appearance
                .get("high_contrast")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );

        let language = query("language")
            .into_iter()
            .find(|row| row.id == "appearance.language")
            .expect("language action");
        let args = ActionArgs::bind(
            &language.params,
            [(
                "language".into(),
                crate::shell::search::action_api::ArgValue::Text("German".into()),
            )],
        )
        .expect("language arguments");
        let mut document = AutoCommit::new();
        (language.on_open)(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .unwrap();
        assert_eq!(
            settings_store::str_at(&settings_store::load(), "appearance", "language"),
            Some("de-DE")
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_theme_action_records_its_action_id_with_both_leaves_it_sets() {
        struct Uninstall;
        impl Drop for Uninstall {
            fn drop(&mut self) {
                settings_store::install_source(None);
            }
        }
        let config = tempfile::tempdir().expect("isolated settings");
        let paths = settings_store::SessionPaths::beside(config.path().join("settings.json"));
        let records = settings_store::SessionSettings::open(paths.clone()).expect("records");
        settings_store::install_source(Some(std::sync::Arc::new(records)));
        let _uninstall = Uninstall;

        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let rows = {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "light theme",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        invoke_action(&rows, "appearance.theme.light", &chrome);

        let log = std::fs::read_to_string(paths.change_log()).expect("change log");
        let entries: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON entry per line"))
            .collect();
        let mut leaves: Vec<_> = entries
            .iter()
            .map(|entry| {
                assert_eq!(entry["section"], "appearance");
                assert_eq!(entry["action"], "appearance.theme.light");
                assert_eq!(entry["via"], "session");
                (
                    entry["leaf"].as_str().unwrap_or_default(),
                    entry["after"].clone(),
                )
            })
            .collect();
        leaves.sort_by_key(|(leaf, _)| *leaf);
        assert_eq!(
            leaves,
            [
                ("high_contrast", serde_json::json!(false)),
                ("mode", serde_json::json!("light")),
            ],
            "one merge sets both leaves, each attributed to the action"
        );
        assert_eq!(
            settings_store::load_at(&paths.copy)["appearance"]["mode"],
            "light",
            "processes reading the copy see the typed write"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn language_action_switches_the_live_catalogue() {
        struct RestoreLocale;
        impl Drop for RestoreLocale {
            fn drop(&mut self) {
                ui::i18n::I18nManager::set_locale("en-US");
            }
        }
        let config = tempfile::tempdir().expect("isolated settings");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        let _restore = RestoreLocale;

        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let language = {
            let state = chrome.borrow();
            let doc = AutoCommit::new();
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "language",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|row| row.id == "appearance.language")
                .expect("language action")
        };
        let args = ActionArgs::bind(
            &language.params,
            [(
                "language".into(),
                crate::shell::search::action_api::ArgValue::Text("German".into()),
            )],
        )
        .expect("language arguments");
        let mut document = AutoCommit::new();
        (language.on_open)(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .unwrap();

        assert_eq!(
            settings_store::str_at(&settings_store::load(), "appearance", "language"),
            Some("de-DE")
        );
        assert_eq!(
            crate::shell::i18n::current_locale(),
            "de-DE",
            "writing the store without selecting the live catalogue leaves chrome English"
        );
        assert_eq!(t("tab-closed-undo"), "Rückgängig");
        assert_eq!(t("search-action-appearance-language"), "Sprache festlegen");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn collaboration_and_agent_rows_appear_only_where_they_can_work() {
        // Phase `collaboration_rows_appear_only_with_sync_and_lead_when_named`.
        // Offered only by a session running peer sync, and when offered,
        // naming one selects it: a label-prefix match outranks other providers.
        {
            let p = ActionsProvider::new();
            let doc = AutoCommit::new();
            let query = |chrome: &ChromeState, needle: &str| {
                p.query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle,
                    wanted: &[ResourceKind::Action],
                    chrome,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
            };

            let off = chrome_rc();
            {
                let cs = off.borrow();
                assert!(
                    query(&cs, "invite").is_empty(),
                    "a session without peer sync must not offer to invite anyone",
                );
            }

            // Invitations go into a room this device governs: a roomless
            // session has nothing to invite into. Without a broker the agent
            // row (also an "Invite …") stays out of this ranking, as it does
            // for a roomless session.
            let on = chrome_with_room();
            on.borrow_mut().peer_sync_enabled = true;
            on.borrow_mut()
                .set_collab_unavailable("this phase ranks the peer rows alone");
            let cs = on.borrow();
            let mut hits = query(&cs, "invite");
            hits.sort_by_key(|r| -r.score);
            assert_eq!(
                hits.first().map(|r| r.id.as_str()),
                Some("peer.invite"),
                "typing \"invite\" must rank the invite command first, got {:?}",
                hits.iter().map(|r| (&r.id, r.score)).collect::<Vec<_>>(),
            );
            assert_eq!(
                hits[0].score, 100,
                "a label-prefix match is the top of this provider's scale, which \
                 is what puts the row above hits from other providers",
            );
            let mut join = query(&cs, "join");
            join.sort_by_key(|r| -r.score);
            assert_eq!(join.first().map(|r| r.id.as_str()), Some("peer.connect"),);
        }

        // Phase `the_agent_row_is_offered_only_where_it_can_work`.
        // Needs a room (the grant is recorded *in* one) and a working broker
        // (how an agent reaches the room). Not peer sync — an agent uses its
        // own broker socket.
        {
            let p = ActionsProvider::new();
            let doc = AutoCommit::new();
            let offered = |chrome: &ChromeState| {
                p.query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "agent",
                    wanted: &[ResourceKind::Action],
                    chrome,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .iter()
                .any(|r| r.id == "agent.invite")
            };

            let roomless = chrome_rc();
            assert!(
                !offered(&roomless.borrow()),
                "a session with no room has nowhere to record the grant, so it \
                 must not offer to admit anyone",
            );

            let broker_down = chrome_with_room();
            broker_down
                .borrow_mut()
                .set_collab_unavailable("the broker could not start");
            assert!(
                !offered(&broker_down.borrow()),
                "an agent reaches the room through the broker; with none there \
                 is nothing for the grant to admit",
            );

            let ready = chrome_with_room();
            assert!(
                offered(&ready.borrow()),
                "a session with a room and a working broker must offer the row",
            );
            assert!(
                !ready.borrow().peer_sync_enabled,
                "and must do so without peer sync, which this row does not need",
            );
        }

        // Phase `invoking_it_admits_the_agent_to_the_room`.
        // Asserted through `agent_members`, the same reader the roster paints
        // from, not a row in a vector this test put there.
        {
            let c = chrome_with_room();

            let before = c
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .expect("fixture has a room")
                .agent_members();
            assert!(
                before.is_empty(),
                "nobody is admitted before the action runs"
            );

            let action = Action::ALL
                .iter()
                .find(|a| a.id == "agent.invite")
                .expect("the action is registered");
            (action.apply)(&mut c.borrow_mut(), &ActionArgs::none())
                .expect("the room's owner may admit an agent");

            let after = c
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .expect("fixture has a room")
                .agent_members();
            assert_eq!(
                after,
                vec![(
                    DEFAULT_AGENT_NAME.to_lowercase(),
                    tilo_action_policy::KindSet::READ
                )],
                "the shipped agent is admitted under its own name, case-folded",
            );
            assert!(
                c.borrow().overlays.ws_settings,
                "and the dedicated workspace Agents roster is shown so the \
                 person who admitted it can review its scope",
            );
        }

        // Phase `the_name_is_case_folded_into_one_principal`.
        // A name typed in any case names one agent. Two spellings would be
        // two subjects, which reads on screen as a grant that did not take.
        {
            let c = chrome_with_room();
            let action = Action::ALL
                .iter()
                .find(|a| a.id == "agent.invite")
                .expect("registered");
            for spelling in ["Scribe", "SCRIBE", "  scribe  "] {
                let args = ActionArgs::bind(
                    AGENT_INVITE_PARAMS,
                    [(
                        "name".to_string(),
                        crate::shell::search::action_api::ArgValue::Text(spelling.to_string()),
                    )],
                )
                .expect("name is a declared text parameter");
                (action.apply)(&mut c.borrow_mut(), &args).expect("owner may admit");
            }
            let members = c
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .expect("fixture has a room")
                .agent_members();
            assert_eq!(
                members.len(),
                1,
                "three spellings of one name must be one agent, got {members:?}",
            );
        }
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the authorization regression retains the chrome guard until each ambient-authority assertion completes"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn privileged_shell_actions_require_admin_authority_and_confirmation() {
        // Phase `non_admin_cannot_manage_identity_devices`.
        {
            use crate::shell::state::{Workspace, WorkspaceId};

            let mut tree = WorkspaceTree::default();
            let id = WorkspaceId::new();
            let mut workspace = Workspace::new(id, "Someone else's workspace");
            workspace.set_owner("a-different-person");
            tree.desktop.workspaces.push(workspace);
            tree.desktop.active = id;
            let chrome =
                ChromeState::new_with_handle(crate::shell::state::WorkspaceTreeHandle::new(tree));

            for action_id in [
                "identity.devices.copy",
                "identity.device.enroll",
                "identity.device.revoke",
            ] {
                let action = Action::ALL
                    .iter()
                    .find(|action| action.id == action_id)
                    .expect("device action is registered");
                let args = if action_id == "identity.device.revoke" {
                    ActionArgs::bind(
                        action.params,
                        [
                            (
                                "device".into(),
                                crate::shell::search::action_api::ArgValue::Text("0".repeat(64)),
                            ),
                            (
                                "confirm".into(),
                                crate::shell::search::action_api::ArgValue::Boolean(true),
                            ),
                        ],
                    )
                    .expect("confirmed revocation arguments bind")
                } else {
                    ActionArgs::none()
                };
                let error = (action.apply)(&mut chrome.borrow_mut(), &args)
                    .expect_err("a non-admin must be refused before device state is touched");
                assert!(
                    matches!(&error, ActionError::Refused(message) if message.contains("may not administer devices")),
                    "unexpected refusal for {action_id}: {error:?}",
                );
            }
        }

        // Phase `setup_profile_is_recorded_but_never_becomes_ambient_authority`.
        {
            let c = chrome_with_room();
            let action = Action::ALL
                .iter()
                .find(|a| a.id == "agent.invite")
                .expect("registered");
            let args = ActionArgs::bind(
                AGENT_INVITE_PARAMS,
                [
                    (
                        "name".to_string(),
                        crate::shell::search::action_api::ArgValue::Text("Operator".to_string()),
                    ),
                    (
                        "preset".to_string(),
                        crate::shell::search::action_api::ArgValue::Text("operator".to_string()),
                    ),
                ],
            )
            .expect("declared setup parameters bind");
            (action.apply)(&mut c.borrow_mut(), &args).expect("owner admits agent");

            let state = c.borrow();
            let tree = state.tree();
            let ws = tree.desktop.active_workspace().expect("fixture has room");
            assert_eq!(
                ws.agent_members()
                    .into_iter()
                    .find(|(agent, _)| agent == "operator")
                    .map(|(_, kinds)| kinds),
                Some(tilo_action_policy::KindSet::ALL),
                "the durable admission carries the selected preset"
            );
            assert!(
                ws.policy
                    .grants
                    .iter()
                    .any(|grant| grant.id.as_str() == "agent-member:operator:operator")
            );
            let answer = |kind| {
                ws.subject_authority(tilo_action_policy::Subject::agent("operator"))
                    .decide(
                        &ws.node().app("so.tilo.notes"),
                        kind,
                        &tilo_desktop_model::context_at(tilo_clock::unix_now()),
                    )
                    .answer
            };
            assert_eq!(
                answer(tilo_action_policy::ActionKind::Undoable),
                tilo_action_policy::Answer::Allow,
                "an Operator makes undoable changes"
            );
            assert_eq!(
                answer(tilo_action_policy::ActionKind::Outside),
                tilo_action_policy::Answer::Ask,
                "and asks first before reaching outside the room"
            );
        }

        // Phase `ownership_actions_are_typed_and_require_explicit_confirmation`.
        {
            let chrome = chrome_with_room();
            let owner = chrome.borrow().active_local_user();
            chrome
                .borrow()
                .tree_mut()
                .desktop
                .active_workspace_mut()
                .expect("fixture has a workspace")
                .set_owner(owner);
            let state = chrome.borrow();
            let doc = AutoCommit::new();
            let results = ActionsProvider::new().query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "ownership",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            let proposal = results
                .iter()
                .find(|result| result.id.ends_with("#ownership.propose"))
                .expect("active workspace governance resource is searchable");
            assert!(proposal.id.starts_with("workspace:"));
            assert_eq!(
                proposal
                    .params
                    .iter()
                    .map(|parameter| parameter.name.as_ref())
                    .collect::<Vec<_>>(),
                ["recipient", "expires_unix"]
            );
            let contract = proposal.contract.as_ref().expect("ownership contract");
            assert_eq!(contract.kind, tilo_actions::ActionKind::Outside);
            assert_eq!(
                contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
            assert!(
                Action::ALL
                    .iter()
                    .all(|action| !action.id.starts_with("ownership.")),
                "the canonical resource must not have a divergent static duplicate"
            );
        }

        // Phase `retained_shell_action_rechecks_authority_before_mutation`.
        {
            let provider = ActionsProvider::new();
            let chrome = chrome_with_room();
            let mut doc = AutoCommit::new();
            let result = {
                let state = chrome.borrow();
                provider
                    .query(SearchContext {
                        query_scope: None,
                        include_programmatic: false,
                        needle: "do not disturb",
                        wanted: &[ResourceKind::Action],
                        chrome: &state,
                        doc: &doc,
                        doc_root: &automerge::ROOT,
                    })
                    .into_iter()
                    .find(|result| result.id == "notifications.dnd.toggle")
                    .expect("authorized action is discovered")
            };
            {
                let state = chrome.borrow_mut();
                let mut tree = state.tree_mut();
                let workspace = tree.desktop.active_workspace_mut().expect("workspace");
                workspace.policy.owner = None;
                workspace.policy.members.clear();
                drop(tree);
            }
            let error = (result.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none(),
            )
            .expect_err("revocation between discovery and execution must win");
            assert!(matches!(error, ActionError::Refused(_)));
            assert!(!chrome.borrow().dnd());
        }

        // Phase `permanent_device_revocation_requires_admin_and_confirmation`.
        {
            let action = Action::ALL
                .iter()
                .find(|action| action.id == "identity.device.revoke")
                .expect("device revocation is registered");
            let contract = shell_action_contract(action.id);
            assert_eq!(contract.kind, tilo_actions::ActionKind::Outside);
            assert_eq!(
                contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
            assert_eq!(contract.risk, tilo_actions::RiskLevel::Privileged);

            let args = ActionArgs::bind(
                action.params,
                [(
                    "device".into(),
                    crate::shell::search::action_api::ArgValue::Text("0".repeat(64)),
                )],
            );
            assert!(
                matches!(args, Err(crate::shell::search::action_api::ArgError::Missing { name }) if name == "confirm"),
                "confirmation is an enforced typed argument, not display-only metadata"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn shell_action_contracts_match_availability_risk_and_mechanism() {
        // Phase `window_actions_are_published_with_focus_and_layout_contracts`.
        {
            for id in WINDOW_FOCUS_ACTION_IDS
                .iter()
                .chain(WINDOW_LAYOUT_ACTION_IDS)
            {
                assert!(
                    Action::ALL.iter().any(|action| action.id == *id),
                    "{id} is listed but not published"
                );
            }
            for id in WINDOW_FOCUS_ACTION_IDS {
                assert_eq!(shell_action_kind(id), tilo_actions::OperationKind::View);
                let contract = shell_action_contract(id);
                assert_eq!(
                    contract.kind,
                    tilo_actions::ActionKind::Read,
                    "{id} only moves focus"
                );
                assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
            }
            for id in WINDOW_LAYOUT_ACTION_IDS {
                assert_eq!(shell_action_kind(id), tilo_actions::OperationKind::Effect);
                let contract = shell_action_contract(id);
                assert_eq!(
                    contract.kind,
                    tilo_actions::ActionKind::Undoable,
                    "{id} rearranges the shared desk"
                );
                assert!(contract.reversible);
                assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
            }
        }

        // Phase `every_shell_action_exposes_a_valid_operational_contract`.
        {
            let chrome = chrome_with_room();
            let state = chrome.borrow();
            let doc = AutoCommit::new();
            let results = ActionsProvider::new().query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            for result in results {
                let contract = result
                    .contract
                    .unwrap_or_else(|| panic!("{} has no contract", result.id));
                assert!(
                    contract.validate_declaration().is_empty(),
                    "{} has invalid contract: {:?}",
                    result.id,
                    contract.validate_declaration()
                );
            }
        }

        // Phase `contracts_match_current_availability_risk_and_document_mutation`.
        {
            let chrome = chrome_with_room();
            let state = chrome.borrow();
            let doc = AutoCommit::new();
            let results = ActionsProvider::new().query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "quick-settings.",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            for id in [
                QS_BRIGHTNESS_ID,
                QS_VOLUME_ID,
                QS_WIFI_ID,
                QS_POWER_ID,
                QS_VPN_ID,
                QS_BLUETOOTH_ID,
                QS_AIRPLANE_ID,
            ] {
                let result = results
                    .iter()
                    .find(|result| result.id == id)
                    .unwrap_or_else(|| panic!("missing Quick Settings effect {id}"));
                assert!(matches!(
                    result
                        .contract
                        .as_ref()
                        .map(|contract| &contract.availability),
                    Some(tilo_actions::Availability::Unavailable { .. })
                ));
            }
            for id in ["identity.connect", "identity.select"] {
                let contract = shell_action_contract(id);
                assert_eq!(contract.risk, tilo_actions::RiskLevel::Privileged);
                assert_eq!(
                    contract.confirmation,
                    tilo_actions::ConfirmationPolicy::Confirm
                );
                assert!(
                    Action::ALL
                        .iter()
                        .find(|action| action.id == id)
                        .expect("identity action")
                        .params
                        .iter()
                        .any(|parameter| parameter.name == "confirm")
                );
            }
            assert!(!quick_settings_contract().reversible);
            assert_eq!(
                shell_action_kind("agent.invite"),
                tilo_actions::OperationKind::Document
            );
            assert_eq!(
                shell_action_kind("peer.invite.named"),
                tilo_actions::OperationKind::Document
            );
        }

        // Phase `quick_settings_contracts_follow_each_production_mechanism`.
        {
            let dispatcher: Rc<dyn crate::shell::quick_settings::actions::QsActions> =
                Rc::new(RecordingQsActions::default());
            let availability = crate::shell::quick_settings::actions::QsActionAvailability {
                brightness: false,
                volume: true,
                wifi: false,
                power_profile: true,
                vpn: false,
                bluetooth: true,
                airplane_mode: false,
            };
            let provider = ActionsProvider::with_shared_qs_actions(Rc::new(
                SharedQsDispatcher::new(dispatcher, availability),
            ));
            let chrome = chrome_with_room();
            let doc = AutoCommit::new();
            let results = provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "quick-settings.",
                wanted: &[ResourceKind::Action],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            for (id, available) in [
                (QS_BRIGHTNESS_ID, false),
                (QS_VOLUME_ID, true),
                (QS_WIFI_ID, false),
                (QS_POWER_ID, true),
                (QS_VPN_ID, false),
                (QS_BLUETOOTH_ID, true),
                (QS_AIRPLANE_ID, false),
            ] {
                let contract = results
                    .iter()
                    .find(|result| result.id == id)
                    .and_then(|result| result.contract.as_ref())
                    .unwrap_or_else(|| panic!("missing contract for {id}"));
                assert_eq!(contract.availability.is_available(), available, "{id}");
            }
        }
    }

    #[derive(Default)]
    struct RecordingQsActions {
        calls: std::cell::RefCell<Vec<String>>,
        fail: std::cell::Cell<bool>,
    }

    impl crate::shell::quick_settings::actions::QsActions for RecordingQsActions {
        fn set_brightness(&self, percent: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("brightness:{percent}"));
            self.result()
        }

        fn set_display_brightness(&self, connector: &str, percent: f32) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("brightness:{connector}:{percent}"));
            self.result()
        }

        fn set_volume(&self, percent: f32) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("volume:{percent}"));
            self.result()
        }

        fn set_wifi(&self, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("wifi:{enabled}"));
            self.result()
        }

        fn set_bluetooth(&self, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("bluetooth:{enabled}"));
            self.result()
        }

        fn set_airplane_mode(&self, enabled: bool) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("airplane:{enabled}"));
            self.result()
        }

        fn set_vpn_connection(&self, name: &str, enabled: bool) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("vpn:{name}:{enabled}"));
            self.result()
        }

        fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
            self.calls
                .borrow_mut()
                .push(format!("connect-wifi:{ssid}:{password:?}"));
            self.result()
        }

        fn set_power_profile(
            &self,
            profile: crate::shell::quick_settings::actions::PowerProfile,
        ) -> Result<(), String> {
            self.calls.borrow_mut().push(format!("power:{profile:?}"));
            self.result()
        }

        fn toggle_mute(&self) -> Result<(), String> {
            self.result()
        }

        fn set_mute(&self, _muted: bool) -> Result<(), String> {
            self.result()
        }
    }

    impl RecordingQsActions {
        fn result(&self) -> Result<(), String> {
            if self.fail.get() {
                Err("controlled provider failure".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn quick_settings_actions_share_one_typed_validated_dispatch() {
        // Phase `typed_quick_settings_effects_use_the_injected_dispatcher_exactly`.
        {
            let recorder = Rc::new(RecordingQsActions::default());
            let dispatcher: Rc<dyn crate::shell::quick_settings::actions::QsActions> =
                recorder.clone();
            let slot = Rc::new(SharedQsDispatcher::new(
                dispatcher,
                crate::shell::quick_settings::actions::QsActionAvailability::all(),
            ));
            let provider = ActionsProvider::with_shared_qs_actions(slot);
            let chrome = chrome_with_room();
            let mut doc = AutoCommit::new();
            let results = provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "quick-settings.",
                wanted: &[ResourceKind::Action],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            });
            let cases = [
                (
                    QS_BRIGHTNESS_ID,
                    vec![(
                        "percent".into(),
                        crate::shell::search::action_api::ArgValue::Number(37.5),
                    )],
                ),
                (
                    QS_VOLUME_ID,
                    vec![(
                        "percent".into(),
                        crate::shell::search::action_api::ArgValue::Integer(64),
                    )],
                ),
                (
                    QS_WIFI_ID,
                    vec![(
                        "enabled".into(),
                        crate::shell::search::action_api::ArgValue::Boolean(true),
                    )],
                ),
                (
                    QS_POWER_ID,
                    vec![(
                        "profile".into(),
                        crate::shell::search::action_api::ArgValue::Text("saver".into()),
                    )],
                ),
                (
                    QS_VPN_ID,
                    vec![
                        (
                            "name".into(),
                            crate::shell::search::action_api::ArgValue::Text("Work".into()),
                        ),
                        (
                            "enabled".into(),
                            crate::shell::search::action_api::ArgValue::Boolean(false),
                        ),
                    ],
                ),
                (
                    QS_BLUETOOTH_ID,
                    vec![(
                        "enabled".into(),
                        crate::shell::search::action_api::ArgValue::Boolean(true),
                    )],
                ),
                (
                    QS_AIRPLANE_ID,
                    vec![(
                        "enabled".into(),
                        crate::shell::search::action_api::ArgValue::Boolean(true),
                    )],
                ),
            ];
            for (id, supplied) in cases {
                let result = results.iter().find(|result| result.id == id).expect(id);
                let args = ActionArgs::bind(&result.params, supplied).expect("typed args bind");
                if let Some(begin) = &result.on_begin_invoke {
                    assert!(matches!(
                        begin(
                            &mut ui::widget::Edits::lend(&mut doc),
                            &automerge::ROOT,
                            &chrome,
                            &args
                        )
                        .expect("controlled provider succeeds"),
                        crate::shell::search::InvocationDisposition::Accepted(_)
                    ));
                } else {
                    (result.on_open)(
                        &mut ui::widget::Edits::lend(&mut doc),
                        &automerge::ROOT,
                        &chrome,
                        &args,
                    )
                    .expect("controlled provider succeeds");
                }
            }
            assert_eq!(
                recorder.calls.borrow().as_slice(),
                [
                    "brightness:37.5",
                    "volume:64",
                    "wifi:true",
                    "power:Saver",
                    "vpn:Work:false",
                    "bluetooth:true",
                    "airplane:true",
                ]
            );
            let state = chrome.borrow();
            assert!(state.quick_settings.bluetooth);
            assert!(state.quick_settings.radios.airplane_mode);
        }

        // Phase `invalid_quick_settings_values_never_dispatch_and_provider_failures_propagate`.
        {
            let recorder = Rc::new(RecordingQsActions::default());
            let params = quick_settings_params(QS_BRIGHTNESS_ID);
            assert!(
                ActionArgs::bind(
                    &params,
                    [(
                        "percent".into(),
                        crate::shell::search::action_api::ArgValue::Number(101.0)
                    )],
                )
                .is_err()
            );
            assert!(recorder.calls.borrow().is_empty());

            recorder.fail.set(true);
            let args = ActionArgs::bind(
                &params,
                [(
                    "percent".into(),
                    crate::shell::search::action_api::ArgValue::Number(50.0),
                )],
            )
            .expect("valid value binds");
            let error = apply_quick_settings_effect(QS_BRIGHTNESS_ID, recorder.as_ref(), &args)
                .expect_err("provider failure reaches the caller");
            assert!(
                matches!(error, ActionError::Refused(message) if message.contains("controlled"))
            );
            assert_eq!(recorder.calls.borrow().as_slice(), ["brightness:50"]);
        }

        // Phase `quick_settings_ui_adapter_and_external_effect_share_typed_dispatch`.
        {
            use crate::shell::quick_settings::actions::QsActions as _;

            let recorder = Rc::new(RecordingQsActions::default());
            let ui = ValidatedQsActions::new(recorder.as_ref());
            let external = |id, supplied| {
                let args = ActionArgs::bind(&quick_settings_params(id), supplied)
                    .expect("valid external value binds");
                apply_quick_settings_effect(id, recorder.as_ref(), &args)
                    .expect("valid external value dispatches");
            };

            ui.set_brightness(37.5).expect("valid UI value dispatches");
            external(
                QS_BRIGHTNESS_ID,
                vec![(
                    "percent".into(),
                    crate::shell::search::action_api::ArgValue::Number(37.5),
                )],
            );
            ui.set_volume(64.0).expect("valid UI value dispatches");
            external(
                QS_VOLUME_ID,
                vec![(
                    "percent".into(),
                    crate::shell::search::action_api::ArgValue::Number(64.0),
                )],
            );
            ui.set_wifi(true).expect("valid UI value dispatches");
            external(
                QS_WIFI_ID,
                vec![(
                    "enabled".into(),
                    crate::shell::search::action_api::ArgValue::Boolean(true),
                )],
            );
            ui.set_power_profile(crate::shell::quick_settings::actions::PowerProfile::Saver)
                .expect("valid UI value dispatches");
            external(
                QS_POWER_ID,
                vec![(
                    "profile".into(),
                    crate::shell::search::action_api::ArgValue::Text("saver".into()),
                )],
            );
            ui.set_vpn_connection("Work", false)
                .expect("valid UI value dispatches");
            external(
                QS_VPN_ID,
                vec![
                    (
                        "name".into(),
                        crate::shell::search::action_api::ArgValue::Text("Work".into()),
                    ),
                    (
                        "enabled".into(),
                        crate::shell::search::action_api::ArgValue::Boolean(false),
                    ),
                ],
            );
            ui.set_bluetooth(true).expect("valid UI value dispatches");
            external(
                QS_BLUETOOTH_ID,
                vec![(
                    "enabled".into(),
                    crate::shell::search::action_api::ArgValue::Boolean(true),
                )],
            );
            assert_eq!(
                recorder.calls.borrow().as_slice(),
                [
                    "brightness:37.5",
                    "brightness:37.5",
                    "volume:64",
                    "volume:64",
                    "wifi:true",
                    "wifi:true",
                    "power:Saver",
                    "power:Saver",
                    "vpn:Work:false",
                    "vpn:Work:false",
                    "bluetooth:true",
                    "bluetooth:true",
                ],
                "every callable UI/external pair reaches the same provider with the same validated value"
            );

            assert!(ui.set_brightness(101.0).is_err());
            assert_eq!(
                recorder.calls.borrow().len(),
                12,
                "invalid UI value never dispatches"
            );
            recorder.fail.set(true);
            assert!(
                ui.set_wifi(true).is_err(),
                "provider failure reaches the UI adapter"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "save then assign through the same headless door, with the decision checked"
    )]
    fn an_agent_can_create_a_grant_set_then_assign_it_through_the_headless_actions() {
        use crate::shell::search::action_api::ArgValue;

        let chrome = chrome_with_room();
        let mut doc = AutoCommit::new();
        let provider = ActionsProvider::new();
        let open_note = tilo_action_policy::TreePath::root()
            .app("so.tilo.notes")
            .resource("notes.page:1")
            .action("note.open");
        let rules = serde_json::to_string(&vec![tilo_action_policy::Rule::new(
            open_note.clone(),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Allow,
        )])
        .unwrap();

        let save = {
            let state = chrome.borrow();
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "preset.save",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id.ends_with("#preset.save"))
                .expect("the role creation action is headlessly discoverable")
        };
        let save_args = ActionArgs::bind(
            &save.params,
            [
                ("name".into(), ArgValue::Text("Notes reader".into())),
                (
                    "description".into(),
                    ArgValue::Text("May inspect individual notes".into()),
                ),
                (
                    "rules".into(),
                    ArgValue::Array(serde_json::from_str(&rules).expect("test rules are JSON")),
                ),
            ],
        )
        .unwrap();
        (save.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &save_args,
        )
        .unwrap();

        let assign = {
            let state = chrome.borrow();
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "assign permission role",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id.ends_with("#preset.assign"))
                .expect("the new grant set immediately becomes assignable")
        };
        let serde_json::Value::Object(researcher) =
            serde_json::to_value(tilo_action_policy::Subject::agent("researcher")).unwrap()
        else {
            panic!("a subject is an object");
        };
        let assign_args = ActionArgs::bind(
            &assign.params,
            [
                ("preset".into(), ArgValue::Text("Notes reader".into())),
                ("subject".into(), ArgValue::Object(researcher)),
            ],
        )
        .unwrap();
        (assign.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &assign_args,
        )
        .unwrap();

        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace().unwrap();
        assert_eq!(
            workspace
                .subject_authority(tilo_action_policy::Subject::agent("researcher"))
                .decide(
                    &workspace.node().join(&open_note),
                    tilo_action_policy::ActionKind::Read,
                    &tilo_desktop_model::context_at(tilo_clock::unix_now()),
                )
                .answer,
            tilo_action_policy::Answer::Allow
        );
        drop(tree);
    }

    fn radio_catalog(
        registry: &crate::shell::search::SearchRegistry,
        chrome: &Chrome,
        doc: &AutoCommit,
    ) -> crate::shell::search::action_api::ActionCatalog {
        crate::shell::search::action_api::ActionCatalog::build(
            registry,
            "quick-settings.",
            crate::shell::search::SearchScope::All,
            None,
            &chrome.borrow(),
            doc,
            &automerge::ROOT,
            &crate::shell::search::action_api::AllVisible,
        )
    }

    fn assert_radio_outcome(
        poll: &crate::shell::search::PendingInvocation,
        chrome: &Chrome,
        change: os_actions::RadioChange,
        outcome: &Result<(), String>,
    ) {
        if outcome.is_ok() {
            let receipt = poll().unwrap().expect("real successful completion");
            assert!(matches!(
                receipt.update,
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
            match change {
                os_actions::RadioChange::Wifi(_) => {
                    assert!(chrome.borrow().quick_settings.wifi);
                }
                os_actions::RadioChange::Airplane(_) => {
                    assert!(chrome.borrow().quick_settings.radios.airplane_mode);
                }
            }
            assert!(chrome.borrow().notifications().is_empty());
        } else {
            assert!(
                matches!(poll(), Err(ActionError::Refused(error)) if error.contains("fixture radio refusal"))
            );
            assert!(!chrome.borrow().quick_settings.wifi);
            assert!(!chrome.borrow().quick_settings.radios.airplane_mode);
            assert_eq!(chrome.borrow().notifications().len(), 1);
            assert!(poll().is_err());
            assert_eq!(chrome.borrow().notifications().len(), 1);
        }
    }

    #[test]
    fn radio_semantic_actions_wait_for_real_work_and_report_success_or_refusal() {
        use crate::shell::quick_settings::radio::fixture::Fixture;
        use crate::shell::search::InvocationDisposition;
        use crate::shell::search::action_api::{ArgValue, ResourceRef};
        for (id, change) in [
            (QS_WIFI_ID, os_actions::RadioChange::Wifi(true)),
            (QS_AIRPLANE_ID, os_actions::RadioChange::Airplane(true)),
        ] {
            for outcome in [Ok(()), Err("fixture radio refusal".into())] {
                let fixture = Fixture::new();
                let slot = Rc::new(SharedQsDispatcher::new(
                    Rc::new(fixture.dispatcher),
                    crate::shell::quick_settings::actions::QsActionAvailability::all(),
                ));
                let (wake_tx, wake_rx) = std::sync::mpsc::channel();
                slot.set_radio_wake(Arc::new(move || {
                    let _ = wake_tx.send(());
                }));
                let mut registry = crate::shell::search::SearchRegistry::new();
                registry.register(Rc::new(ActionsProvider::with_shared_qs_actions(
                    slot.clone(),
                )));
                let chrome = chrome_with_room();
                chrome.borrow_mut().quick_settings.wifi = false;
                chrome.borrow_mut().quick_settings.radios.airplane_mode = false;
                let mut doc = AutoCommit::new();
                let catalog = radio_catalog(&registry, &chrome, &doc);
                let resource = ResourceRef::new("actions", id);
                let action = catalog.select(&resource).expect("declared Wi-Fi action");
                let disposition = action
                    .invoke(
                        &mut ui::widget::Edits::lend(&mut doc),
                        &automerge::ROOT,
                        &chrome,
                        [("enabled".into(), ArgValue::Boolean(true))],
                    )
                    .expect("owner action is admitted");
                let InvocationDisposition::Pending(poll) = disposition else {
                    panic!("starting radio work cannot manufacture completion");
                };
                assert_eq!(
                    fixture
                        .entered
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap(),
                    change
                );
                assert!(poll().unwrap().is_none());
                assert!(!chrome.borrow().quick_settings.wifi);
                let busy_catalog = radio_catalog(&registry, &chrome, &doc);
                assert!(matches!(
                    busy_catalog
                        .select(&resource)
                        .unwrap()
                        .contract()
                        .unwrap()
                        .availability,
                    tilo_actions::Availability::Unavailable { .. }
                ));
                assert!(
                    action
                        .invoke(
                            &mut ui::widget::Edits::lend(&mut doc),
                            &automerge::ROOT,
                            &chrome,
                            [("enabled".into(), ArgValue::Boolean(false))],
                        )
                        .is_err(),
                    "a stale descriptor cannot admit a second radio mutation"
                );
                fixture.release.send(outcome.clone()).unwrap();
                wake_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                assert_radio_outcome(&poll, &chrome, change, &outcome);
                assert!(!chrome.borrow().quick_settings.radio_change_pending);
                let completion = slot
                    .take_radio_completion()
                    .expect("host consumes same owned receipt");
                completion.apply(&chrome);
                assert!(slot.take_radio_completion().is_none());
                let restored = radio_catalog(&registry, &chrome, &doc);
                assert!(matches!(
                    restored
                        .select(&resource)
                        .unwrap()
                        .contract()
                        .unwrap()
                        .availability,
                    tilo_actions::Availability::Available
                ));
            }
        }
    }

    #[test]
    fn radio_semantic_admission_rechecks_current_dispatcher_and_workspace_authority() {
        use crate::shell::quick_settings::radio::fixture::Fixture;
        use crate::shell::search::action_api::{ArgValue, ResourceRef};
        let fixture = Fixture::new();
        let slot = Rc::new(SharedQsDispatcher::new(
            Rc::new(fixture.dispatcher.clone()),
            crate::shell::quick_settings::actions::QsActionAvailability::all(),
        ));
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(Rc::new(ActionsProvider::with_shared_qs_actions(
            slot.clone(),
        )));
        let chrome = chrome_with_room();
        let mut doc = AutoCommit::new();
        let catalog = radio_catalog(&registry, &chrome, &doc);
        let action = catalog
            .select(&ResourceRef::new("actions", QS_WIFI_ID))
            .unwrap();
        slot.replace(
            Rc::new(fixture.dispatcher.clone()),
            crate::shell::quick_settings::actions::QsActionAvailability::default(),
        );
        assert!(
            action
                .invoke(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &chrome,
                    [("enabled".into(), ArgValue::Boolean(true))]
                )
                .is_err()
        );
        assert!(fixture.entered.try_recv().is_err());
        slot.replace(
            Rc::new(fixture.dispatcher.clone()),
            crate::shell::quick_settings::actions::QsActionAvailability::all(),
        );
        chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .unwrap()
            .set_owner("foreign-owner");
        assert!(
            action
                .invoke(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &chrome,
                    [("enabled".into(), ArgValue::Boolean(true))]
                )
                .is_err()
        );
        assert!(
            fixture.entered.try_recv().is_err(),
            "revoked authority never reaches the native worker"
        );
    }

    const FIXTURE_USER: &str = "shell-fixture";

    /// [`chrome_with_room`] with `member` admitted as an Editor through the
    /// production granting path, and the room document seeded from it.
    fn chrome_with_member(member: &str) -> Chrome {
        let chrome = chrome_with_room();
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let ws = tree.desktop.active_workspace_mut().unwrap();
            ws.govern_as_at(
                FIXTURE_USER,
                tilo_action_policy::Change::AddMember {
                    person: member.to_owned(),
                    role: tilo_action_policy::Role::Editor,
                },
                tilo_clock::unix_now(),
            )
            .expect("the owner may admit a member");
            tree.seed_doc_from_cache();
        }
        chrome
    }

    /// Invoke one shell action the way the control socket does: resolve its
    /// exact address in a catalog, then bind and run its JSON arguments.
    /// Every action stays selectable, so the provider's own authority
    /// check is the one under test.
    fn invoke_shell_action(
        chrome: &Chrome,
        id: &str,
        args: &serde_json::Value,
    ) -> Result<crate::shell::search::InvocationDisposition, ActionError> {
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(Rc::new(ActionsProvider::new()));
        let mut doc = AutoCommit::new();
        let resource = ResourceRef::new("actions", id);
        let catalog = ActionCatalog::build_exact(
            &registry,
            &resource,
            crate::shell::search::SearchScope::All,
            None,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        let action = catalog
            .select(&resource)
            .ok_or_else(|| ActionError::Refused(format!("{id} is not published")))?;
        action.invoke_json(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            chrome,
            args.as_object().expect("arguments are an object"),
        )
    }

    fn role_of(chrome: &Chrome, member: &str) -> Option<crate::shell::state::CapRole> {
        let state = chrome.borrow();
        let tree = state.tree();
        let role = tree
            .desktop
            .active_workspace()
            .and_then(|ws| ws.role_of(member));
        drop(tree);
        role
    }

    /// The member's role as the replicated room document records it.
    fn replicated_role_of(chrome: &Chrome, member: &str) -> Option<crate::shell::state::CapRole> {
        let state = chrome.borrow();
        let tree = state.tree();
        let mut from_doc = tilo_desktop_model::DesktopTree::default();
        tilo_desktop_model::desktop_doc::rebuild(&tree.doc, &mut from_doc);
        let active = tree.desktop.active;
        drop(tree);
        from_doc
            .workspaces
            .iter()
            .find(|ws| ws.id == active)
            .and_then(|ws| ws.role_of(member))
    }

    /// The active room's whole authority: its policy and its sandbox.
    fn capabilities(
        chrome: &Chrome,
    ) -> Option<(
        tilo_action_policy::RoomPolicy,
        Vec<crate::shell::state::CapOverride>,
    )> {
        let state = chrome.borrow();
        let tree = state.tree();
        let caps = tree
            .desktop
            .active_workspace()
            .map(|ws| (ws.policy.clone(), ws.sandbox.clone()));
        drop(tree);
        caps
    }

    #[test]
    fn member_role_set_action_changes_role_through_workspace_caps() {
        use crate::shell::state::CapRole;
        // Phase `the_owner_changes_a_members_role_in_the_cache_and_the_room_document`.
        {
            let chrome = chrome_with_member("bob");
            assert_eq!(role_of(&chrome, "bob"), Some(CapRole::Editor));
            let disposition = invoke_shell_action(
                &chrome,
                MEMBER_ROLE_SET_ID,
                &serde_json::json!({"participant_id": "bob", "role": "viewer", "confirm": true}),
            )
            .expect("the owner may change a member's role");
            assert!(matches!(
                disposition,
                crate::shell::search::InvocationDisposition::Accepted(_)
            ));
            assert_eq!(role_of(&chrome, "bob"), Some(CapRole::Viewer));
            assert_eq!(
                replicated_role_of(&chrome, "bob"),
                Some(CapRole::Viewer),
                "the change reached the replicated room document, not only the cache"
            );
        }

        // Phase `an_unconfirmed_or_unknown_change_changes_nothing`.
        {
            let chrome = chrome_with_member("bob");
            let before = capabilities(&chrome);
            assert!(
                invoke_shell_action(
                    &chrome,
                    MEMBER_ROLE_SET_ID,
                    &serde_json::json!({"participant_id": "bob", "role": "viewer"}),
                )
                .is_err(),
                "a privileged change needs explicit confirmation"
            );
            assert!(
                invoke_shell_action(
                    &chrome,
                    MEMBER_ROLE_SET_ID,
                    &serde_json::json!({"participant_id": "bob", "role": "owner", "confirm": true}),
                )
                .is_err(),
                "ownership is not a role this action can hand out"
            );
            assert!(
                invoke_shell_action(
                    &chrome,
                    MEMBER_ROLE_SET_ID,
                    &serde_json::json!({"participant_id": "carol", "role": "editor", "confirm": true}),
                )
                .is_err(),
                "setting a role is not a way to admit someone who was never invited"
            );
            assert_eq!(capabilities(&chrome), before);
        }

        // Phase `a_delegated_administrator_passes_the_same_capability_check`.
        {
            let chrome = chrome_with_member("bob");
            {
                let state = chrome.borrow();
                let mut tree = state.tree_mut();
                let ws = tree.desktop.active_workspace_mut().unwrap();
                ws.set_owner("foreign-owner");
                let now = tilo_clock::unix_now();
                ws.govern_as_at(
                    "foreign-owner",
                    tilo_action_policy::Change::AddMember {
                        person: FIXTURE_USER.to_owned(),
                        role: tilo_action_policy::Role::Editor,
                    },
                    now,
                )
                .expect("the owner admits the delegate");
                let set_role = tilo_action_policy::roles::governance_action(
                    &ws.room.to_string(),
                    tilo_action_policy::roles::governance::SET_ROLE,
                );
                ws.govern_as_at(
                    "foreign-owner",
                    tilo_action_policy::Change::Grant(tilo_action_policy::Grant::new(
                        "delegate-set-role",
                        tilo_action_policy::Subject::person(FIXTURE_USER),
                        set_role,
                        tilo_action_policy::KindSet::ALL,
                        tilo_action_policy::Answer::Allow,
                        tilo_action_policy::Subject::person("foreign-owner"),
                    )),
                    now,
                )
                .expect("the owner delegates administration");
                drop(tree);
            }
            invoke_shell_action(
                &chrome,
                MEMBER_ROLE_SET_ID,
                &serde_json::json!({"participant_id": "bob", "role": "commenter", "confirm": true}),
            )
            .expect("administration delegated as a capability is administration");
            assert_eq!(role_of(&chrome, "bob"), Some(CapRole::Commenter));
        }
    }

    #[test]
    fn member_remove_action_refuses_non_admin_and_owner() {
        // Phase `the_owner_removes_a_member`.
        {
            let chrome = chrome_with_member("bob");
            invoke_shell_action(
                &chrome,
                MEMBER_REMOVE_ID,
                &serde_json::json!({"participant_id": "bob", "confirm": true}),
            )
            .expect("the owner may remove a member");
            assert_eq!(role_of(&chrome, "bob"), None);
            assert_eq!(replicated_role_of(&chrome, "bob"), None);
        }

        // Phase `the_recorded_owner_cannot_be_removed`.
        {
            let chrome = chrome_with_member("bob");
            let before = capabilities(&chrome);
            assert!(
                invoke_shell_action(
                    &chrome,
                    MEMBER_REMOVE_ID,
                    &serde_json::json!({"participant_id": FIXTURE_USER, "confirm": true}),
                )
                .is_err()
            );
            assert_eq!(capabilities(&chrome), before);
        }

        // Phase `a_non_administrator_is_refused_by_the_action_and_by_membership`.
        {
            let chrome = chrome_with_member("bob");
            chrome
                .borrow_mut()
                .tree_mut()
                .desktop
                .active_workspace_mut()
                .unwrap()
                .set_owner("foreign-owner");
            let before = capabilities(&chrome);
            assert!(matches!(
                invoke_shell_action(
                    &chrome,
                    MEMBER_REMOVE_ID,
                    &serde_json::json!({"participant_id": "bob", "confirm": true}),
                ),
                Err(ActionError::Refused(_))
            ));
            assert_eq!(
                crate::shell::membership::remove_member(
                    &mut chrome.borrow_mut(),
                    &"bob".to_string()
                ),
                Err(crate::shell::membership::MembershipError::Unauthorized(
                    MEMBER_REMOVE_ID
                )),
                "the Members tab's own entry point refuses a non-administrator too"
            );
            assert_eq!(
                crate::shell::membership::set_member_role(
                    &mut chrome.borrow_mut(),
                    &"bob".to_string(),
                    crate::shell::state::CapRole::Viewer,
                ),
                Err(crate::shell::membership::MembershipError::Unauthorized(
                    MEMBER_ROLE_SET_ID
                ))
            );
            assert_eq!(capabilities(&chrome), before);
        }
    }

    #[test]
    fn invitation_params_follow_the_canonical_definitions() {
        let definitions = tilo_action_broker::collaboration_admin::action_definitions();
        for (id, canonical) in [
            (MEMBER_ROLE_SET_ID, MEMBER_ROLE_SET_ID),
            (MEMBER_REMOVE_ID, MEMBER_REMOVE_ID),
            (INVITATION_ISSUE_ID, INVITATION_ISSUE_ID),
            (INVITATION_CANCEL_ID, INVITATION_CANCEL_ID),
            (INVITATION_LIST_ID, INVITATION_LIST_ID),
            (INVITATION_RESEND_ID, INVITATION_CANCEL_ID),
        ] {
            let action = Action::ALL
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("{id} is published"));
            let definition = definitions
                .iter()
                .find(|definition| definition.id == canonical)
                .unwrap_or_else(|| panic!("{canonical} is canonical"));
            let shared: Vec<_> = action
                .params
                .iter()
                .filter(|param| !membership_actions::SHELL_ONLY_PARAMS.contains(&&*param.name))
                .cloned()
                .collect();
            assert_eq!(
                shared, definition.params,
                "{id} declares the canonical parameters"
            );
            assert!(
                action.params.iter().any(|param| param.name == "confirm"),
                "{id} carries the confirmation its contract demands"
            );
            let contract = shell_action_contract(id);
            assert_eq!(
                contract.kind,
                tilo_actions::ActionKind::Outside,
                "{id} is administration"
            );
            if id != INVITATION_RESEND_ID {
                assert_eq!(contract.risk, definition.contract.risk);
            }
            assert_eq!(contract.result, definition.contract.result);
            assert_eq!(
                contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
            assert!(
                contract.validate_declaration().is_empty(),
                "{id}: {:?}",
                contract.validate_declaration()
            );
            assert_eq!(shell_action_kind(id), tilo_actions::OperationKind::Effect);
        }
        assert!(
            !membership_actions::INVITATION_ACTION_IDS.contains(&MEMBER_ROLE_SET_ID)
                && !membership_actions::INVITATION_ACTION_IDS.contains(&MEMBER_REMOVE_ID)
        );
    }

    #[test]
    fn membership_actions_are_published_and_invitations_need_peer_sync() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let rows = rows_for(&provider, &chrome, "");
        assert!(row(&rows, MEMBER_ROLE_SET_ID).is_some());
        assert!(row(&rows, MEMBER_REMOVE_ID).is_some());
        for id in membership_actions::INVITATION_ACTION_IDS {
            assert!(row(&rows, id).is_none(), "{id} needs peer sync");
        }
        chrome.borrow_mut().peer_sync_enabled = true;
        let rows = rows_for(&provider, &chrome, "");
        for id in membership_actions::INVITATION_ACTION_IDS {
            assert!(row(&rows, id).is_some(), "{id} is published with peer sync");
        }
    }

    #[test]
    fn invitation_requests_are_validated_before_anything_is_queued() {
        use crate::shell::overlay_host::{InvitationTerms, PeerSyncRequest};
        let bind = |id: &str, args: serde_json::Value| {
            let action = Action::ALL.iter().find(|action| action.id == id).unwrap();
            let args = ActionArgs::bind_json(action.params, args.as_object().unwrap()).unwrap();
            invitation_request(id, &args)
        };
        let recipient = "ab".repeat(32);
        assert_eq!(
            bind(
                INVITATION_ISSUE_ID,
                serde_json::json!({
                    "role": "commenter",
                    "expires_in_seconds": 600,
                    "invitee_name": "  Bob ",
                    "recipient": format!("tilo-identity:{recipient}"),
                    "confirm": true,
                }),
            )
            .unwrap(),
            PeerSyncRequest::IssueInvitation(InvitationTerms {
                role: crate::network::invite::InviteRole::Commenter,
                expires_in_seconds: 600,
                invitee_name: Some("Bob".into()),
                recipient_user_public_key: Some([0xab; 32]),
            })
        );
        for refused in [
            serde_json::json!({"role": "editor", "expires_in_seconds": 0, "confirm": true}),
            serde_json::json!({
                "role": "editor",
                "expires_in_seconds": membership_actions::MAX_INVITATION_LIFETIME_SECONDS.saturating_add(1),
                "confirm": true,
            }),
            serde_json::json!({"role": "owner", "expires_in_seconds": 60, "confirm": true}),
            serde_json::json!({
                "role": "editor",
                "expires_in_seconds": 60,
                "recipient": "not-a-key",
                "confirm": true,
            }),
        ] {
            assert!(
                bind(INVITATION_ISSUE_ID, refused.clone()).is_err(),
                "{refused} is refused"
            );
        }
        let id = "07".repeat(32);
        assert_eq!(
            bind(
                INVITATION_CANCEL_ID,
                serde_json::json!({"invite_id": id, "confirm": true})
            )
            .unwrap(),
            PeerSyncRequest::CancelInvitation([7; 32])
        );
        assert_eq!(
            bind(
                INVITATION_RESEND_ID,
                serde_json::json!({"invite_id": id, "confirm": true})
            )
            .unwrap(),
            PeerSyncRequest::ResendInvitation([7; 32])
        );
        assert!(
            bind(
                INVITATION_CANCEL_ID,
                serde_json::json!({"invite_id": "07", "confirm": true})
            )
            .is_err()
        );
        assert_eq!(
            bind(INVITATION_LIST_ID, serde_json::json!({"confirm": true})).unwrap(),
            PeerSyncRequest::ListPendingInvitations
        );
    }
}
