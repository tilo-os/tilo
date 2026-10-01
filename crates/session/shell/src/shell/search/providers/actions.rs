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
mod text_insert;

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

pub(crate) fn shell_action_contract(id: &str) -> tilo_actions::ActionContract {
    use tilo_actions::{
        ActionContract, ConfirmationPolicy, PermissionRequirement, RetryPolicy, RiskLevel,
    };

    let mut contract = ActionContract::default();
    match id {
        "shortcut-help.toggle" | "overview.toggle" => {
            contract = ActionContract::view();
            contract.retry = RetryPolicy::AtMostOnce;
            contract.reversible = true;
        }
        "desktop-tour.open"
        | "settings.open"
        | "settings.members.open"
        | "settings.privacy.open"
        | "settings.apps.open"
        | "quick-settings.open"
        | "quick-settings.clipboard"
        | "quick-settings.devices"
        | "projection.open"
        | "capture.open"
        | "collaborators.open"
        | "room.activity.open"
        | "room.activity.explore"
        | "room.activity.replay" => {
            contract = ActionContract::view();
        }
        "identity.connect" | "identity.select" | "identity.device.revoke" => {
            contract.permission = PermissionRequirement::Admin;
            contract.risk = RiskLevel::Privileged;
            contract.confirmation = ConfirmationPolicy::Confirm;
        }
        "identity.devices.copy" | "identity.device.enroll" | "identity.home-peer.enroll" => {
            contract.permission = PermissionRequirement::Admin;
        }
        "peer.invite.named" | "agent.invite" => {
            contract.permission = PermissionRequirement::Invite;
        }
        "peer.invite" | "peer.connect" => {
            contract = ActionContract::view();
            contract.permission = PermissionRequirement::Invite;
        }
        // Moving a tab between groups is layout, undone by moving it back;
        // a close is undone by `tab.reopen`, which puts it back in place.
        "text.insert"
        | "record.bring"
        | "tab.new.terminal.here"
        | "window.new.terminal"
        | "tab.duplicate"
        | "tab.split-right"
        | "tab.move-to-new-group"
        | "tab.close"
        | "tab.reopen" => {
            contract.permission = PermissionRequirement::Write;
            contract.reversible = true;
        }
        "notifications.dnd.toggle"
        | "appearance.theme.dark"
        | "appearance.theme.light"
        | "appearance.high-contrast"
        | "appearance.language" => {
            contract.permission = PermissionRequirement::Write;
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
            contract.permission = PermissionRequirement::Write;
            contract.retry = RetryPolicy::AtMostOnce;
        }
        // Each is one step: repeating it steps again.
        id if WINDOW_FOCUS_ACTION_IDS.contains(&id) => {
            contract = ActionContract::view();
            contract.retry = RetryPolicy::AtMostOnce;
            contract.reversible = true;
        }
        // Layout, undone by the opposite move.
        id if WINDOW_LAYOUT_ACTION_IDS.contains(&id) => {
            contract.permission = PermissionRequirement::Write;
            contract.retry = RetryPolicy::AtMostOnce;
            contract.reversible = true;
        }
        _ => {}
    }
    contract
}

pub(crate) fn shell_action_kind(id: &str) -> tilo_actions::OperationKind {
    match id {
        "shortcut-help.toggle"
        | "desktop-tour.open"
        | "overview.toggle"
        | "settings.open"
        | "settings.members.open"
        | "settings.privacy.open"
        | "settings.apps.open"
        | "quick-settings.open"
        | "quick-settings.clipboard"
        | "quick-settings.devices"
        | "projection.open"
        | "capture.open"
        | "collaborators.open"
        | "room.activity.open"
        | "room.activity.explore"
        | "room.activity.replay"
        | "peer.invite"
        | "peer.connect" => tilo_actions::OperationKind::View,
        "text.insert" | "record.bring" | "peer.invite.named" | "agent.invite" => {
            tilo_actions::OperationKind::Document
        }
        _ if id.contains("#ownership.") => tilo_actions::OperationKind::Document,
        _ if WINDOW_FOCUS_ACTION_IDS.contains(&id) => tilo_actions::OperationKind::View,
        _ => tilo_actions::OperationKind::Effect,
    }
}

fn current_shell_action_authorized(state: &ChromeState, id: &str) -> bool {
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
    let Some((target, verb)) =
        crate::shell::search::visibility::required_capability(&resource, &kind)
    else {
        return true;
    };
    workspace.allows(actor, &target, verb)
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
            workspace.may_administer(&actor),
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

#[expect(
    clippy::too_many_lines,
    reason = "one dispatcher over every workspace governance action; each arm validates and applies a single operation"
)]
fn apply_permission_profile_action(
    state: &ChromeState,
    action_id: &str,
    arguments: &ActionArgs,
) -> Result<(), ActionError> {
    let actor = state.active_local_user();
    let mut tree = state.tree_mut();
    let (workspace_id, room) = tree
        .desktop
        .active_workspace()
        .map(|workspace| (workspace.id, workspace.room))
        .ok_or_else(|| ActionError::Refused("no active workspace".into()))?;
    if !tree.governs_room(room) {
        return Err(ActionError::Refused(
            "this device is not the workspace governance authority".into(),
        ));
    }
    match action_id {
        "profile.save" => {
            let rules = serde_json::from_value::<Vec<collab_types::PermissionProfileRule>>(
                serde_json::Value::Array(arguments.array("rules").unwrap_or_default().to_vec()),
            )
            .map_err(|error| ActionError::Refused(format!("invalid permission rules: {error}")))?;
            let profile = collab_types::PermissionProfile {
                name: arguments.text("name").unwrap_or_default().to_string(),
                description: arguments
                    .text("description")
                    .unwrap_or_default()
                    .to_string(),
                rules,
            };
            tree.desktop
                .active_workspace_mut()
                .ok_or_else(|| ActionError::Refused("no active workspace".into()))?
                .save_permission_profile_as(&actor, profile.clone())
                .map_err(|error| ActionError::Refused(error.to_string()))?;
            tree.doc_upsert_permission_profile(workspace_id, &profile);
        }
        "profile.assign" => {
            let profile = arguments
                .text("profile")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| ActionError::Refused("permission role is missing".into()))?;
            let subject_id = arguments.text("subject").unwrap_or_default().trim();
            if subject_id.is_empty() {
                return Err(ActionError::Refused(
                    "choose a collaborator, app, or agent identity".into(),
                ));
            }
            let subject = match arguments
                .text("subject_kind")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "person" => collab_types::Subject::person(subject_id),
                "app" => collab_types::Subject::app(subject_id),
                "agent" => collab_types::Subject::agent(subject_id),
                _ => {
                    return Err(ActionError::Refused(
                        "subject_kind must be person, app, or agent".into(),
                    ));
                }
            };
            let expiry = arguments
                .integer("expires_unix")
                .and_then(|value| u64::try_from(value).ok())
                .filter(|value| *value > 0)
                .map_or(collab_types::Expiry::Never, collab_types::Expiry::AtUnix);
            let ids = tree
                .desktop
                .active_workspace_mut()
                .ok_or_else(|| ActionError::Refused("no active workspace".into()))?
                .assign_permission_profile_as(&actor, profile, &subject, expiry)
                .map_err(|error| ActionError::Refused(error.to_string()))?;
            let written = tree
                .desktop
                .active_workspace()
                .map(|workspace| {
                    workspace
                        .capabilities
                        .iter()
                        .filter(|capability| ids.contains(&capability.id))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for capability in written {
                tree.doc_insert_capability(workspace_id, &capability);
            }
        }
        tilo_desktop::workspace_settings::APP_ACTION_GRANT_ACTION_ID
        | tilo_desktop::workspace_settings::APP_ACTION_REVOKE_ACTION_ID => {
            use tilo_desktop::workspace_settings::PermissionProfileOperation;
            let text = |name: &str| arguments.text(name).unwrap_or_default().to_string();
            let operation =
                if action_id == tilo_desktop::workspace_settings::APP_ACTION_GRANT_ACTION_ID {
                    PermissionProfileOperation::GrantAppAction {
                        app: text("app"),
                        target_app: text("target_app"),
                        action: text("action"),
                        lifetime: text("lifetime"),
                    }
                } else {
                    PermissionProfileOperation::RevokeAppAction {
                        app: text("app"),
                        target_app: text("target_app"),
                        action: text("action"),
                    }
                };
            let person = tree
                .desktop
                .active_workspace()
                .ok_or_else(|| ActionError::Refused("no active workspace".into()))?
                .actor_for_person(&actor);
            crate::shell::workspace_governance::apply_app_action_operation(
                &mut tree,
                workspace_id,
                &person,
                &operation,
            )
            .map_err(ActionError::Refused)?;
        }
        _ => {
            return Err(ActionError::Refused(
                "unknown permission role action".into(),
            ));
        }
    }
    tree.flush_to_disk();
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
        .and_then(|workspace| workspace.owner.as_deref())
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
    let (workspace_id, room, actor, may_administer) = {
        let tree = state.tree();
        let Some(workspace) = tree.desktop.active_workspace() else {
            return Err(ActionError::Refused("no active workspace".into()));
        };
        let actor = state.local_user_for_room(&workspace.room);
        (
            workspace.id,
            workspace.room,
            actor.clone(),
            workspace.may_administer(&actor),
        )
    };
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

/// Current wall-clock time as nanoseconds since the epoch, clamped to
/// `u64::MAX` rather than truncated — a value that large is centuries past
/// any real expiry/enrollment window this crate computes against.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to 0..=u64::MAX immediately above, so it always fits u64"
)]
fn now_ns_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
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

/// Parameters of `window.focus-nth`.
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

fn persist_appearance(
    st: &mut ChromeState,
    values: serde_json::Map<String, serde_json::Value>,
) -> Result<(), ActionError> {
    settings_store::update_section("appearance", values)
        .map_err(|err| ActionError::Refused(err.to_string()))?;
    crate::shell::i18n::apply_configured_locale();
    st.note_appearance_change();
    Ok(())
}

fn set_theme(st: &mut ChromeState, theme: &str, high_contrast: bool) -> Result<(), ActionError> {
    let mut values = serde_json::Map::new();
    values.insert("theme".into(), serde_json::Value::String(theme.into()));
    values.insert(
        "high_contrast".into(),
        serde_json::Value::Bool(high_contrast),
    );
    persist_appearance(st, values)
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
    let mut values = serde_json::Map::new();
    values.insert(
        "language".into(),
        serde_json::Value::String(language.into()),
    );
    persist_appearance(st, values)
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
                    tree.grant_in_workspace(
                        workspace_id,
                        &actor,
                        crate::shell::state::CapOverride::grant(
                            crate::shell::state::Subject::person(profile.principal_id()),
                            crate::shell::state::Scope::Workspace,
                            crate::shell::state::Verb::Admin,
                        ),
                    )
                    .map(|_| ())
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
                    if tree
                        .grant_in_workspace(
                            workspace_id,
                            &actor,
                            crate::shell::state::CapOverride::grant(
                                crate::shell::state::Subject::person(profile.principal_id()),
                                crate::shell::state::Scope::Workspace,
                                crate::shell::state::Verb::Admin,
                            ),
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
                let now = now_ns_u64();
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
                let now = now_ns_u64();
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
                let now = now_ns_u64();
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
                // Mint the token too, so naming and inviting are one
                // gesture; minted *for* them so the card names whose invite it is.
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::IssueInviteFor {
                        name,
                        copy: false,
                    },
                );
                Ok(())
            },
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
                use crate::shell::state::{CapOverride, Subject};
                use tilo_desktop::workspace_settings::AgentAuthorityPreset;
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
                    Some(value) => AgentAuthorityPreset::parse(value).ok_or_else(|| {
                        ActionError::Refused(
                            "choose Observer, Contributor, Operator, or Custom".into(),
                        )
                    })?,
                    None => AgentAuthorityPreset::Observer,
                };
                let user = st.active_local_user();
                let granted = {
                    let mut tree = st.tree_mut();
                    let Some((ws_id, room)) = tree
                        .desktop
                        .active_workspace()
                        .map(|w| (w.id, w.room.to_string()))
                    else {
                        return Err(ActionError::Refused(
                            "there is no workspace here to add an agent to".into(),
                        ));
                    };
                    // An ordinary grant on the room, through the tree so it
                    // reaches the room document (and other members) and disk.
                    // The profile is only an admission marker. The agent's
                    // actor has no room role, so it receives no implicit
                    // authority until the owner reviews a concrete surface.
                    let cap = CapOverride::membership(
                        Subject::agent(agent.clone()),
                        room,
                        preset.admission_verb(),
                    )
                    .with_id(format!("agent-member:{agent}:{}", preset.id()));
                    tree.grant_in_workspace(ws_id, &user, cap)
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
        // The tab menu's three rows as actions, on the focused tab: a
        // capability reachable only by right-click is not finished. The
        // rows and these closures call the same `window_actions` functions.
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
            |st, _| set_theme(st, "dark", false),
        ),
        chrome(
            "appearance.theme.light",
            "search-action-appearance-theme-light",
            "search-action-appearance-theme-light-subtitle",
            |st, _| set_theme(st, "light", false),
        ),
        chrome(
            "appearance.high-contrast",
            "search-action-appearance-high-contrast",
            "search-action-appearance-high-contrast-subtitle",
            |st, _| set_theme(st, "dark", true),
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
        // The first exported action that changes a document rather than
        // the chrome around one, and so the first told *what* to write.
        Action {
            id: text_insert::ACTION_ID,
            label: "search-action-text-insert",
            subtitle: "search-action-text-insert-subtitle",
            params: text_insert::PARAMS,
            apply: text_insert::apply,
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
}

impl SharedQsDispatcher {
    pub(crate) fn new(
        actions: Rc<dyn crate::shell::quick_settings::actions::QsActions>,
        availability: crate::shell::quick_settings::actions::QsActionAvailability,
    ) -> Self {
        Self {
            actions: std::cell::RefCell::new(actions),
            availability: std::cell::Cell::new(availability),
        }
    }

    pub(crate) fn replace(
        &self,
        actions: Rc<dyn crate::shell::quick_settings::actions::QsActions>,
        availability: crate::shell::quick_settings::actions::QsActionAvailability,
    ) {
        *self.actions.borrow_mut() = actions;
        self.availability.set(availability);
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
fn stage_display_layout(st: &mut ChromeState, mode: DisplayLayoutMode) -> Result<(), ActionError> {
    settings_store::display_mode::stage_layout(mode)
        .map_err(|error| ActionError::Refused(error.to_string()))?;
    st.overlays.projection = true;
    Ok(())
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
        permission: tilo_actions::PermissionRequirement::Write,
        retry: tilo_actions::RetryPolicy::Idempotent,
        // An unconfirmed layout restores itself; Revert undoes it at once.
        reversible: true,
        ..tilo_actions::ActionContract::default()
    }
}

fn quick_settings_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        permission: tilo_actions::PermissionRequirement::Write,
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
        self.query(ctx).into_iter().find(|result| result.id == id)
    }
    #[expect(
        clippy::too_many_lines,
        reason = "the catalog lists every shell action with its contract in one table-like pass"
    )]
    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
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
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let mut out = Vec::new();
        // Every row is built with its searchable text; ranking them together
        // then keeps the matches and scores them.
        let mut candidates = ActionCandidates::default();
        for action in Action::ALL {
            // A session not running peer sync has nothing to invite into.
            if action.id.starts_with("peer.") && !ctx.chrome.peer_sync_enabled {
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
                        |_doc: &mut automerge::AutoCommit,
                         _root: &automerge::ObjId,
                         _chrome: &Chrome,
                         _args: &ActionArgs| {
                            Err(ActionError::Refused(
                                "record.bring requires lifecycle dispatch".into(),
                            ))
                        },
                    ) as Rc<crate::shell::search::OpenFn>;
                    let begin = Rc::new(
                        |_doc: &mut automerge::AutoCommit,
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
                        move |_doc: &mut automerge::AutoCommit,
                              _root: &automerge::ObjId,
                              _chrome: &Chrome,
                              _args: &ActionArgs| {
                            Err(ActionError::Refused(format!(
                                "{action_id} requires lifecycle dispatch"
                            )))
                        },
                    ) as Rc<crate::shell::search::OpenFn>;
                    let begin = Rc::new(
                        move |_doc: &mut automerge::AutoCommit,
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
                            let chrome = chrome.clone();
                            let ticket: crate::shell::search::PendingInvocation =
                                Rc::new(move || {
                                    match chrome
                                        .borrow_mut()
                                        .take_peer_sync_request_result(request_id)
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
                } else {
                    let action_id = action.id;
                    let on_open = Rc::new(
                        move |_doc: &mut automerge::AutoCommit,
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
        for &(id, label_key, subtitle_key) in QUICK_SETTINGS_ACTIONS {
            let label = t(label_key);
            let subtitle = t(subtitle_key);
            candidates.record(&label.to_lowercase(), &subtitle.to_lowercase(), id);
            let score = 0;
            let actions = self.qs_actions.actions.borrow().clone();
            let available = self.qs_actions.action_available(id);
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
                contract: Some(quick_settings_contract_for(available)),
                on_open: Rc::new(move |_doc, _root, chrome, args| {
                    if !current_shell_action_authorized(&chrome.borrow(), id) {
                        return Err(ActionError::Refused(format!(
                            "current workspace authority no longer permits {id}"
                        )));
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
                on_begin_invoke: None,
            });
        }
        let actor = ctx.chrome.active_local_user();
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
                tilo_desktop::workspace_settings::permission_profile_resource(workspace, &actor)
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
                    let action_id = action.id.clone();
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
                            apply_permission_profile_action(&state, &action_id, arguments)?;
                            state.bump();
                            Ok(())
                        }),
                        on_invoke: None,
                        on_begin_invoke: None,
                    });
                }
            }
        }
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
            if let Some(catalog) = &self.provider_catalog
                && asks_about_search_sources(ctx.needle)
            {
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
            }
        }
        let actor = ctx.chrome.active_local_user();
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
                tilo_desktop::workspace_settings::permission_profile_resource(workspace, &actor)
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
                    let action_id = action.id.clone();
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
                            apply_permission_profile_action(&state, &action_id, arguments)?;
                            state.bump();
                            Ok(())
                        }),
                        on_invoke: None,
                        on_begin_invoke: None,
                    });
                }
            }
        }
        crate::shell::search::rank_rows(&self.ranker, ctx.needle, out, &candidates.0)
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
    use parking_lot::RwLock;
    use std::sync::Arc;

    fn chrome_rc() -> crate::shell::chrome_state::Chrome {
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(WorkspaceTree::default()));
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
        let handle: WorkspaceTreeHandle = Arc::new(RwLock::new(tree));
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
        (result.on_open)(&mut document, &automerge::ROOT, chrome, &ActionArgs::none()).unwrap();
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
        begin(&mut document, &automerge::ROOT, chrome, &ActionArgs::none()).unwrap()
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
            (action.on_open)(&mut doc, &automerge::ROOT, &c, &ActionArgs::none())
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
            let fire = |doc: &mut AutoCommit| {
                (dnd.on_open)(doc, &automerge::ROOT, &c, &ActionArgs::none())
                    .expect("a nullary chrome action must run");
            };
            fire(&mut doc);
            assert!(c.borrow().dnd(), "opening the action turns DND on");
            fire(&mut doc);
            assert!(!c.borrow().dnd(), "opening it again turns DND back off");
        }

        // Phase `tab_duplicate_failure_reaches_the_invoker_and_keeps_user_feedback`.
        {
            let action = Action::ALL
                .iter()
                .find(|action| action.id == "tab.duplicate")
                .expect("duplicate action is registered");
            let chrome = ChromeState::new_with_handle(std::sync::Arc::new(
                parking_lot::RwLock::new(crate::shell::state::WorkspaceTree::default()),
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
                &mut doc,
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

    #[test]
    fn workspace_activity_exact_local_actions_are_exported() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let out = provider.query(SearchContext {
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
        ] {
            assert!(
                out.iter().any(|action| action.id == id
                    && action.operation_kind == tilo_actions::OperationKind::View),
                "{id}"
            );
        }
    }

    #[test]
    fn history_finds_activity_views_and_not_a_workspace_switch() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let query = |needle| {
            provider.query(SearchContext {
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

            (result.on_open)(&mut doc, &automerge::ROOT, &c, &ActionArgs::none()).unwrap();

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

            (action.on_open)(&mut doc, &automerge::ROOT, &chrome, &args).unwrap();

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

            (result.on_open)(&mut doc, &automerge::ROOT, &chrome, &ActionArgs::none()).unwrap();
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
        for action in Action::ALL {
            for key in [action.label, action.subtitle] {
                let copy = t(key);
                assert_ne!(copy, key, "{key} has no entry in the catalogue");
                assert!(!copy.trim().is_empty(), "{key} resolves to nothing");
            }
        }
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
            appearance.get("theme").and_then(serde_json::Value::as_str),
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
        (language.on_open)(&mut document, &automerge::ROOT, &chrome, &args).unwrap();
        assert_eq!(
            settings_store::str_at(&settings_store::load(), "appearance", "language"),
            Some("de-DE")
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
        (language.on_open)(&mut document, &automerge::ROOT, &chrome, &args).unwrap();

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

            let on = chrome_rc();
            on.borrow_mut().peer_sync_enabled = true;
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
                    crate::shell::state::CapRole::Viewer
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
            let chrome = ChromeState::new_with_handle(Arc::new(RwLock::new(tree)));

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
                ws.role_of_agent("operator"),
                Some(crate::shell::state::CapRole::Editor),
                "the durable admission marker carries the selected setup intent"
            );
            assert!(
                ws.capabilities
                    .iter()
                    .any(|cap| { cap.id == "agent-member:operator:operator" })
            );
            assert!(
                !ws.allows(
                    &ws.actor_for_agent("operator"),
                    &crate::shell::state::ResourceRef::App("so.tilo.notes".into()),
                    crate::shell::state::Verb::Write,
                ),
                "even the Operator profile requires an explicit scoped grant"
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
            assert_eq!(
                contract.permission,
                tilo_actions::PermissionRequirement::Admin
            );
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
                workspace.owner = None;
                workspace.capabilities.clear();
                drop(tree);
            }
            let error = (result.on_open)(&mut doc, &automerge::ROOT, &chrome, &ActionArgs::none())
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
            assert_eq!(
                contract.permission,
                tilo_actions::PermissionRequirement::Admin
            );
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
                    contract.permission,
                    tilo_actions::PermissionRequirement::Read,
                    "{id} only moves focus"
                );
                assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
            }
            for id in WINDOW_LAYOUT_ACTION_IDS {
                assert_eq!(shell_action_kind(id), tilo_actions::OperationKind::Effect);
                let contract = shell_action_contract(id);
                assert_eq!(
                    contract.permission,
                    tilo_actions::PermissionRequirement::Write,
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
                (result.on_open)(&mut doc, &automerge::ROOT, &chrome, &args)
                    .expect("controlled provider succeeds");
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
    fn an_agent_can_create_a_role_then_assign_it_through_the_headless_actions() {
        use crate::shell::search::action_api::ArgValue;

        let chrome = chrome_with_room();
        let mut doc = AutoCommit::new();
        let provider = ActionsProvider::new();
        let rules = serde_json::to_string(&vec![collab_types::PermissionProfileRule::grant(
            collab_types::Scope::Action {
                app: "so.tilo.notes".into(),
                action: "note.open".into(),
            },
            collab_types::Verb::Read,
        )])
        .unwrap();

        let save = {
            let state = chrome.borrow();
            provider
                .query(SearchContext {
                    needle: "profile.save",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id.ends_with("#profile.save"))
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
        (save.on_open)(&mut doc, &automerge::ROOT, &chrome, &save_args).unwrap();

        let assign = {
            let state = chrome.borrow();
            provider
                .query(SearchContext {
                    needle: "assign permission role",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id.ends_with("#profile.assign"))
                .expect("the new role immediately becomes assignable")
        };
        let assign_args = ActionArgs::bind(
            &assign.params,
            [
                ("profile".into(), ArgValue::Text("Notes reader".into())),
                ("subject".into(), ArgValue::Text("researcher".into())),
                ("subject_kind".into(), ArgValue::Text("agent".into())),
            ],
        )
        .unwrap();
        (assign.on_open)(&mut doc, &automerge::ROOT, &chrome, &assign_args).unwrap();

        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace().unwrap();
        assert!(workspace.allows(
            &workspace.actor_for_agent("researcher"),
            &crate::shell::state::ResourceRef::Action {
                app: "so.tilo.notes".into(),
                action: "note.open".into(),
            },
            crate::shell::state::Verb::Read,
        ));
        drop(tree);
    }
}
