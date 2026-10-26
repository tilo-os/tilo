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

mod device_pairing;
mod enrollment_tokens;
mod identity_admin;
mod native_identity;
mod quick_settings;
mod shortcuts;
mod strips;
mod tabs;
mod windows;
mod workspaces;
pub(crate) use quick_settings::{
    QS_AIRPLANE_ID, QS_BLUETOOTH_ID, QS_BRIGHTNESS_ID, QS_POWER_ID, QS_VOLUME_ID, QS_VPN_ID,
    QS_WIFI_ID, SharedQsActions, SharedQsDispatcher, ValidatedQsActions,
};
use quick_settings::{apply_quick_settings_effect, quick_settings_params, validated_percent};
use shortcuts::ACTION_SHORTCUTS;
pub(crate) use shortcuts::action_shortcut;

use strips::{
    STRIP_CREATE_PARAMS, STRIP_MOVE_PARAMS, STRIP_REMOVE_PARAMS, STRIP_RENAME_PARAMS, create_strip,
    move_strip, remove_strip, rename_strip,
};
pub(crate) use strips::{STRIP_NAME_MAX_CHARS, valid_strip_name};
use tabs::{
    TAB_MOVE_TO_WORKSPACE_PARAMS, TAB_RENAME_PARAMS, move_focused_tab_to_workspace,
    rename_focused_tab,
};
use windows::{
    GROUP_MOVE_TO_STRIP_ID, GROUP_MOVE_TO_STRIP_PARAMS, group_move_to_strip_params,
    move_group_to_strip, no_window, request_group_shortcut, window_outcome,
};
use workspaces::{
    WORKSPACE_DELETE_PARAMS, delete_workspace, move_workspace, workspace_delete_params,
};

use crate::shell::shared_display_actions;

const WORKSPACE_CREATE_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "name",
    ParamType::Text,
    "1 to 120 characters without control characters. Omit to use the localized default name.",
)];

fn workspace_create_context(
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
    let workspace = tree
        .desktop
        .active_workspace()
        .ok_or_else(|| ActionError::Refused("there is no active workspace context".into()))?;
    let context = (
        workspace.id,
        workspace.room(),
        state.local_user_for_room(&workspace.room()),
    );
    drop(tree);
    Ok(context)
}

fn create_workspace(state: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    let default_name = t("search-action-workspace-create-default-name");
    let supplied = args.text("name").unwrap_or(&default_name);
    let name = supplied.trim();
    if name.is_empty() || name.chars().count() > 120 || supplied.chars().any(char::is_control) {
        return Err(ActionError::Explained(t(
            "search-action-workspace-create-invalid-name",
        )));
    }
    state.add_workspace(name);
    Ok(())
}

/// What `workspace.rename` must be told.
const WORKSPACE_RENAME_PARAMS: &[ActionParam] = &[ActionParam::required(
    "name",
    ParamType::Text,
    "The workspace's new name: 1 to 120 characters without control characters.",
)];

/// Rename the workspace on screen, as its name field in the workspace menu
/// and its settings does.
fn rename_workspace(state: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    let supplied = args.text("name").unwrap_or_default();
    let name = supplied.trim();
    if name.is_empty() || name.chars().count() > 120 || supplied.chars().any(char::is_control) {
        return Err(ActionError::Explained(t(
            "search-action-workspace-create-invalid-name",
        )));
    }
    let refused = || ActionError::Refused(t("search-action-workspace-rename-refused"));
    if !state.may_rename_active_workspace() {
        return Err(refused());
    }
    state.rename_active_workspace(name);
    let renamed = state
        .tree()
        .desktop
        .active_workspace()
        .is_some_and(|workspace| workspace.name == name);
    if renamed { Ok(()) } else { Err(refused()) }
}

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

/// What `agent.remove` is told: which agent, and the confirmation the
/// shell door requires of a destructive action.
const AGENT_REMOVE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "name",
        ParamType::Text,
        "The agent's name, as the Agents tab lists it.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Confirm this workspace administration request.",
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
        "The identity to use in this workspace.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Confirm delegating workspace administration to this identity.",
    ),
];

/// The parameters `action` asks for now. Choosing the identity a workspace
/// uses offers the identities this device has, each by its name and avatar;
/// nobody types a fingerprint. Choosing a strip offers the active
/// workspace's strips by name, and a deletion's confirmation reads as what
/// it does.
fn live_params(action: &Action, state: &ChromeState) -> Vec<ActionParam> {
    match action.id {
        "identity.select" => {}
        GROUP_MOVE_TO_STRIP_ID => return group_move_to_strip_params(state),
        "workspace.delete" => return workspace_delete_params(),
        _ => return action.params.to_vec(),
    }
    let identities = crate::identity_registry::IdentityRegistryStore::from_env()
        .and_then(|store| store.load_reconciled_cached())
        .map(|registry| {
            registry
                .identities
                .values()
                .map(|profile| (profile.id.clone(), identity_label(&profile.display_name)))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    identity_select_params(&identities)
}

/// The choice that sets up this device's first identity, offered when it
/// has none.
pub(crate) const IDENTITY_SETUP_CHOICE: &str = "setup";

/// [`IDENTITY_SELECT_PARAMS`], its identity one of `identities`
/// (`(id, name)`), each read as its name with its avatar. With none, the one
/// choice is the next step: "Set up your identity", which needs no
/// confirmation, since it delegates nothing yet.
pub(crate) fn identity_select_params(identities: &[(String, String)]) -> Vec<ActionParam> {
    if identities.is_empty() {
        let id = IDENTITY_SELECT_PARAMS
            .iter()
            .find(|param| param.name == "id")
            .cloned()
            .map(|mut param| {
                param.description = t("search-action-identity-none").into();
                param.with_labelled_choices([(
                    IDENTITY_SETUP_CHOICE,
                    t("search-action-identity-setup"),
                )])
            });
        return id.into_iter().collect();
    }
    IDENTITY_SELECT_PARAMS
        .iter()
        .map(|param| {
            if param.name != "id" {
                return param.clone();
            }
            param
                .clone()
                .with_labelled_choices(identities.iter().cloned())
                .choosing_people(identities.iter().map(|(id, _)| id.clone()))
        })
        .collect()
}

/// Ask for the first-identity setup: only on a device with no identity, as
/// the chooser offers it.
fn request_identity_setup(st: &mut ChromeState) -> Result<(), ActionError> {
    let has_identity = crate::identity_registry::IdentityRegistryStore::from_env()
        .and_then(|store| store.load_reconciled())
        .is_ok_and(|registry| !registry.identities.is_empty());
    if has_identity {
        return Err(ActionError::Refused(
            "this device has an identity already; choose it".into(),
        ));
    }
    st.overlays.identity_setup = true;
    Ok(())
}

/// How an identity reads in the chooser: its name, or "Unnamed identity"
/// for one minted before it had one.
fn identity_label(display_name: &str) -> String {
    let name = display_name.trim();
    if name.is_empty() {
        t("search-action-identity-unnamed")
    } else {
        name.to_owned()
    }
}

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

const PRESENCE_AVAILABILITY_PARAMS: &[ActionParam] = &[ActionParam::required(
    "availability",
    ParamType::Text,
    "available, focusing, or away.",
)];

const PRESENCE_VISIBILITY_PARAMS: &[ActionParam] = &[ActionParam::required(
    "visibility",
    ParamType::Text,
    "What the people in this workspace see of you: where (your window and pointer), here (only that you are here), or nothing.",
)];

const DEVICE_REVOKE_PARAMS: &[ActionParam] = crate::shell::identity_admin::REVOKE_PARAMS;

fn workspace_create_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        retry: tilo_actions::RetryPolicy::AtMostOnce,
        ..tilo_actions::ActionContract::default()
    }
}

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
    "settings.open",
    "settings.members.open",
    "settings.privacy.open",
    "settings.apps.open",
    "quick-settings.open",
    "quick-settings.clipboard",
    "quick-settings.devices",
    "quick-settings.network",
    "quick-settings.bluetooth",
    "notifications.open",
    "projection.open",
    "projection.close",
    "capture.open",
    "collaborators.open",
    "room.activity.open",
    "room.activity.explore",
    "room.activity.replay",
    "room.activity.acknowledge",
    "tab.menu",
    "tab.drag.cancel",
    "room.activity.morning.toggle",
    "history.compare",
    "history.undo",
    "history.restore",
    shared_display_actions::INSPECT_ID,
    DISPLAY_OUTPUTS_INSPECT_ID,
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
        }
        id if VIEW_ACTION_IDS.contains(&id) => contract = ActionContract::view(),
        "workspace.create" | "workspace.rename" | "strip.create" | "strip.rename"
        | "strip.move" | "strip.remove" | "tab.rename" => {
            contract = workspace_create_contract();
        }
        "identity.connect" | "identity.select" | "identity.device.revoke" => {
            contract.kind = ActionKind::Outside;
            contract.risk = RiskLevel::Privileged;
            contract.confirmation = ConfirmationPolicy::Confirm;
        }
        id if enrollment_tokens::role_of(id).is_some() => contract = enrollment_tokens::contract(),
        id if device_pairing::is_pair_action(id) => contract = device_pairing::contract(id),
        id if identity_admin::is_identity_action(id) => contract = identity_admin::contract(id),
        "identity.devices.copy" | "peer.invite.named" | "agent.invite" => {
            contract.kind = ActionKind::Outside;
        }
        // Who may walk in is the room's business on its host; a report
        // reaches the people who run a workspace. Each is asked first, as
        // every privileged action is.
        WORKSPACE_PUBLISH_ID | WORKSPACE_UNPUBLISH_ID | WORKSPACE_REPORT_ID | BOT_ADD_ID => {
            contract.kind = ActionKind::Outside;
            contract.risk = RiskLevel::Privileged;
            contract.confirmation = ConfirmationPolicy::Confirm;
        }
        // Its host stops the bot and uninstalls it; adding it again is a
        // new request.
        // Ends the bot's or the agent's access at once; undone only by
        // admitting it again.
        // The app leaves the device; what of the person's it held goes to
        // the Trash, never further.
        // The workspace leaves the person's list on every device; a room
        // another device hosts is left, and joined again only through it.
        BOT_REMOVE_ID
        | "agent.remove"
        | APP_UNINSTALL_ID
        | APP_REMOVE_FROM_APPS_ID
        | "workspace.delete" => {
            contract.kind = ActionKind::Outside;
            contract.risk = RiskLevel::Destructive;
            contract.confirmation = ConfirmationPolicy::Confirm;
            contract.retry = RetryPolicy::AtMostOnce;
        }
        "peer.invite" | "peer.connect" | "peer.pair-device" => {
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
        | "tab.move-to-workspace"
        | "tab.close"
        | "group.close"
        | "tab.reopen" => {
            contract.kind = ActionKind::Undoable;
        }
        // Clearing notifications is undone by bringing them back while the
        // undo lasts.
        "notifications.clear"
        | "notifications.clear.undo"
        | "notifications.dnd.toggle"
        | "appearance.theme.dark"
        | "appearance.theme.light"
        | "appearance.language" => {
            contract.kind = ActionKind::Undoable;
            contract.retry = RetryPolicy::AtMostOnce;
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
        | DISPLAY_REVERT_ID
        | shared_display_actions::INCLUDE_ID => contract = display_layout_contract(),
        BLUETOOTH_PAIRING_DECLINE_ID => {
            contract.kind = ActionKind::Undoable;
            contract.retry = RetryPolicy::AtMostOnce;
        }
        // Navigation and focus each advance one step when repeated.
        id if id.starts_with("navigation.axis.") || WINDOW_FOCUS_ACTION_IDS.contains(&id) => {
            contract = ActionContract::view();
            contract.retry = RetryPolicy::AtMostOnce;
        }
        // Layout, and the order of the person's workspaces, undone by the
        // opposite move.
        id if WINDOW_LAYOUT_ACTION_IDS.contains(&id)
            || matches!(id, "workspace.move-earlier" | "workspace.move-later") =>
        {
            contract.kind = ActionKind::Undoable;
            contract.retry = RetryPolicy::AtMostOnce;
        }
        _ => {}
    }
    contract
}

pub(crate) fn shell_action_kind(id: &str) -> tilo_actions::OperationKind {
    match id {
        "shortcut-help.toggle"
        | "overview.toggle"
        | "peer.invite"
        | "peer.connect"
        | "peer.pair-device" => tilo_actions::OperationKind::View,
        id if VIEW_ACTION_IDS.contains(&id) => tilo_actions::OperationKind::View,
        "record.bring" | "peer.invite.named" | "agent.invite" | "agent.remove"
        | "workspace.create" | "workspace.rename" | "strip.create" | "strip.rename"
        | "strip.move" | "strip.remove" | "tab.rename" => tilo_actions::OperationKind::Document,
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
    let user = state.local_user_for_room(&workspace.room());
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
        let actor = state.local_user_for_room(&workspace.room());
        (
            workspace.room(),
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
        .map(|workspace| workspace.room().to_string())
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
    let actor = state.local_user_for_room(&workspace.room());
    let may_administer = workspace.may_govern_at(
        &actor,
        tilo_action_policy::roles::governance::GRANT,
        tilo_clock::unix_now(),
    );
    let (workspace_id, room) = (workspace.id, workspace.room());
    drop(tree);
    if !may_administer {
        return Err(ActionError::Refused(format!(
            "{actor} may not change this workspace's identity"
        )));
    }
    Ok((workspace_id, room, actor))
}

/// The pairing steps answer through a ticket, never through a plain apply.
fn pairing_needs_lifecycle() -> ActionError {
    ActionError::Refused("device pairing requires lifecycle dispatch".into())
}

fn identity_needs_lifecycle() -> ActionError {
    ActionError::Refused("identity administration requires lifecycle dispatch".into())
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
            .map(crate::shell::state::Workspace::room)
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

/// What `peer.invite.named` must be told. Both required — an invite to
/// nobody in particular is [`peer.invite`].
/// Making the active workspace public, with its description and the role
/// people who join from the directory get.
pub(crate) const WORKSPACE_PUBLISH_ID: &str = "workspace.publish";
/// Making it private again.
pub(crate) const WORKSPACE_UNPUBLISH_ID: &str = "workspace.unpublish";
/// Reporting the public workspace shown to the people who run it.
pub(crate) const WORKSPACE_REPORT_ID: &str = "workspace.report";
/// Asking the active workspace's host to run a bot from the directory.
pub(crate) const BOT_ADD_ID: &str = "bot.add";
/// Taking the active workspace's request for a bot back: its host stops
/// and uninstalls it.
pub(crate) const BOT_REMOVE_ID: &str = "bot.remove";

const BOT_ADD_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "entry",
        ParamType::Text,
        "The bot's entry in the registry's directory, such as so.tilo.bots.standup.",
    ),
    ActionParam::required(
        "version",
        ParamType::Text,
        "The listed version the workspace's host runs.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Add this bot to the workspace: its host runs it, with what it may reach.",
    ),
];

const BOT_REMOVE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "entry",
        ParamType::Text,
        "The bot's entry in the registry's directory.",
    ),
    ActionParam::required(
        "confirm",
        ParamType::Boolean,
        "Remove this bot: its host stops it.",
    ),
];

/// Uninstalling an app from the person's apps ([`crate::shell::app_list`]).
pub(crate) const APP_UNINSTALL_ID: &str = "app.uninstall";
/// Taking an app off the person's apps without uninstalling it: a part of
/// the system, or a downloaded program, whose file stays.
pub(crate) const APP_REMOVE_FROM_APPS_ID: &str = "app.remove-from-apps";

const APP_UNINSTALL_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "app",
        ParamType::Text,
        "The app: a Tilo package id, a desktop-file id or a Flatpak id, as Files' Applications place and search list it.",
    ),
    ActionParam::optional(
        "remove_data",
        ParamType::Boolean,
        "For a Flatpak: move its data (~/.var/app/<id>) to the Trash too. On unless set to false.",
    ),
    ActionParam::optional(
        "confirm",
        ParamType::Boolean,
        "Uninstall now. Without it the person is asked first, in the uninstall sheet.",
    ),
];

const APP_REMOVE_FROM_APPS_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "app",
        ParamType::Text,
        "The app, a part of the system or a downloaded program (whose file stays), as Files' Applications place and search list it.",
    ),
    ActionParam::optional(
        "confirm",
        ParamType::Boolean,
        "Take it off the person's apps now. Without it the person is asked first.",
    ),
];

/// `app.uninstall` and `app.remove-from-apps`: one action wherever it is
/// asked for. Without `confirm` the person is asked in the shell's sheet,
/// as Files, search and the Store ask; with it, it runs.
fn uninstall_app(args: &ActionArgs, remove_from_apps: bool) -> Result<(), ActionError> {
    use os_actions::AppRemoval;
    let id = args.text("app").unwrap_or_default().trim().to_owned();
    if id.is_empty() {
        return Err(ActionError::Refused("name the app".into()));
    }
    // Taking a downloaded program's entry off the person's apps leaves its
    // file where it is and can be undone by adding it again: nothing to ask.
    if remove_from_apps
        && let Some(item) = crate::shell::app_list::find(&id)
        && matches!(
            item.source,
            crate::shell::app_list::AppSource::Downloaded { .. }
        )
    {
        return crate::shell::app_list::remove_from_apps(item).map_err(ActionError::Refused);
    }
    if args.boolean("confirm") != Some(true) {
        crate::shell::app_list::ask(&id);
        return Ok(());
    }
    let Some(item) = crate::shell::app_list::find(&id) else {
        return Err(ActionError::Refused(
            if crate::shell::app_list::settling() {
                format!("the app list is still being read; ask again for {id}")
            } else {
                format!("{id} is not in the person's apps")
            },
        ));
    };
    match (item.removal(), remove_from_apps) {
        (AppRemoval::RemoveFromApps, false) => {
            return Err(ActionError::Refused(format!(
                "{} is part of the system and cannot be uninstalled; {APP_REMOVE_FROM_APPS_ID} takes it off the person's apps",
                item.name
            )));
        }
        (AppRemoval::Uninstall | AppRemoval::UninstallWithData, true) => {
            return Err(ActionError::Refused(format!(
                "{} can be uninstalled; {APP_UNINSTALL_ID} does that",
                item.name
            )));
        }
        (AppRemoval::None, _) => {
            return Err(ActionError::Refused(format!(
                "{} comes with the system and stays",
                item.name
            )));
        }
        _ => {}
    }
    let remove_data = args.boolean("remove_data").unwrap_or(true);
    crate::shell::app_list::uninstall(item, remove_data).map_err(ActionError::Refused)
}

/// The label of the system action `id` when an app may prepare it for the
/// person ([`ui::search::SearchOffer`]): only adding a bot, which the person
/// still confirms in the palette.
pub(crate) fn app_offerable_label(id: &str) -> Option<String> {
    (id == BOT_ADD_ID).then(|| t("search-action-bot-add"))
}

/// The bot `args` ask the active workspace for (or take back), as `ask`
/// builds it from the entry and version.
fn bot_ask(
    args: &ActionArgs,
    ask: fn(String, Option<String>) -> crate::shell::network::BotAsk,
) -> Result<crate::shell::network::BotAsk, ActionError> {
    let entry = args.text("entry").unwrap_or_default().trim().to_owned();
    let version = args
        .text("version")
        .map(|version| version.trim().to_owned());
    let ask = ask(entry, version);
    ask.command().validate().map_err(|_| {
        ActionError::Refused("a bot is named by its directory entry and version".into())
    })?;
    Ok(ask)
}

const WORKSPACE_PUBLISH_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "description",
        ParamType::Text,
        "What the workspace is, in a sentence or two (at most 280 characters).",
    ),
    ActionParam::optional(
        "default_role",
        ParamType::Text,
        "What people who join get: viewer, commenter (when omitted) or editor.",
    ),
];

const WORKSPACE_REPORT_PARAMS: &[ActionParam] = &[ActionParam::required(
    "reason",
    ParamType::Text,
    "What is wrong with this workspace.",
)];

/// The listing `args` publish the active workspace with: their description
/// and role, else the role it is public with now (a fresh one: commenter).
fn publish_listing(
    state: &ChromeState,
    args: &ActionArgs,
) -> Result<tilo_relay_protocol::directory::Listing, ActionError> {
    use tilo_relay_protocol::rooms::RoomRole;
    let current = state
        .tree()
        .desktop
        .active_workspace()
        .map(crate::shell::state::Workspace::room)
        .and_then(|room| match state.publication(room) {
            Some(crate::shell::chrome_state::PublicationState::Known(known)) => {
                Some(known.default_role)
            }
            _ => None,
        });
    let default_role = match args.text("default_role").unwrap_or_default().trim() {
        "" => current.unwrap_or(RoomRole::Commenter),
        "viewer" => RoomRole::Viewer,
        "commenter" => RoomRole::Commenter,
        "editor" => RoomRole::Editor,
        other => {
            return Err(ActionError::Refused(format!(
                "people who join become viewers, commenters or editors, not {other}"
            )));
        }
    };
    tilo_relay_protocol::directory::Listing {
        description: args
            .text("description")
            .unwrap_or_default()
            .trim()
            .to_owned(),
        default_role,
    }
    .checked()
    .map_err(|error| ActionError::Refused(error.to_string()))
}

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

/// Parameters of `strip.focus-nth`.
const STRIP_FOCUS_NTH_PARAMS: &[ActionParam] = &[ActionParam::required(
    "index",
    ParamType::Integer,
    "Which strip of the active workspace, counting from 1 at the top (1 to 9).",
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
    "strip.focus-nth",
];

/// Window-management actions that rearrange the desk. Each tells its
/// caller once the overlay host ran it whether anything changed
/// ([`window_outcome`]).
const WINDOW_LAYOUT_ACTION_IDS: &[&str] = &[
    "group.move-left",
    "group.move-right",
    "group.move-to-strip-previous",
    "group.move-to-strip-next",
    GROUP_MOVE_TO_STRIP_ID,
    "group.move-to-display-previous",
    "group.move-to-display-next",
    "group.maximize",
    "group.restore",
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

/// Dark, light or automatic mode, keeping whichever theme the person chose
/// for it, changed through the appearance section's field edits on behalf
/// of the action `invoked_by`. Choosing a mode leaves high contrast.
pub(super) fn set_mode(
    st: &mut ChromeState,
    mode: &str,
    invoked_by: &settings_store::InvokingAction,
) -> Result<(), ActionError> {
    use crate::shell::settings_records::{address, change, set_field};
    let edit = address("settings.appearance", "field-edit");
    change(&edit, &set_field("mode", &mode.into()), false, invoked_by)?;
    change(
        &edit,
        &set_field("high_contrast", &false.into()),
        false,
        invoked_by,
    )?;
    st.note_appearance_change();
    Ok(())
}

fn set_language(st: &mut ChromeState, requested: &str) -> Result<(), ActionError> {
    use crate::shell::settings_records::{address, change, set_field};
    let language = match requested.trim() {
        "en" | "en-US" | "english" | "English" => "en-US",
        "de" | "de-DE" | "german" | "German" | "deutsch" | "Deutsch" => "de-DE",
        other => {
            return Err(ActionError::Refused(format!(
                "{other:?} is not a language this desktop has"
            )));
        }
    };
    change(
        &address("settings.appearance", "field-edit"),
        &set_field("language", &language.into()),
        false,
        &settings_store::InvokingAction::action("appearance.language"),
    )?;
    crate::shell::i18n::apply_configured_locale();
    st.note_appearance_change();
    Ok(())
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
        // "emoji" or a lone ":" in search: the same picker Super+. opens.
        chrome(
            "emoji.picker",
            "search-action-emoji-picker",
            "search-action-emoji-picker-subtitle",
            |st, _| request_window_shortcut(st, ShortcutAction::EmojiPicker),
        ),
        // The same switch Super+Alt+Space makes, on the keyboard last typed on.
        chrome(
            "input.source.next",
            "shortcut-name-next-input-source",
            "search-action-next-input-source-subtitle",
            |st, _| {
                st.switch_to_next_input_source()
                    .map(drop)
                    .ok_or_else(|| ActionError::Explained(t("input-source-next-none")))
            },
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
                    return Err(ActionError::Explained(t("axis-navigation-disabled")));
                }
                active_workspace_id(st)?;
                st.request_axis_gather();
                Ok(())
            },
        ),
        chrome(
            "feedback.open",
            "search-action-feedback-open",
            "search-action-feedback-open-subtitle",
            |st, _| crate::shell::feedback_entry::open(st),
        ),
        chrome(
            "feedback.point",
            "search-action-feedback-point",
            "search-action-feedback-point-subtitle",
            |st, _| crate::shell::feedback_entry::point(st),
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
                let identity_id = args.text("id").unwrap_or_default().trim();
                if identity_id == IDENTITY_SETUP_CHOICE {
                    return request_identity_setup(st);
                }
                require_explicit_confirmation(args, "select this identity")?;
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
                // The clipboard is out of sight and search closes on choose,
                // so confirm in passing rather than with a notification.
                ui::text_selection::clipboard_copy(&lines);
                st.acknowledge(t("search-action-device-list-copied"));
                Ok(())
            },
        ),
        Action {
            id: enrollment_tokens::DEVICE_ENROLL_ID,
            label: "search-action-identity-device-enroll",
            subtitle: "search-action-identity-device-enroll-subtitle",
            params: enrollment_tokens::ENROLL_PARAMS,
            apply: |st, args| {
                enrollment_tokens::mint_for_person(
                    st,
                    args,
                    tilo_identity_host::enrollment::EnrollmentRole::PersonalDevice,
                )
            },
        },
        Action {
            id: enrollment_tokens::HOME_PEER_ENROLL_ID,
            label: "search-action-identity-home-peer-enroll",
            subtitle: "search-action-identity-home-peer-enroll-subtitle",
            params: enrollment_tokens::ENROLL_PARAMS,
            apply: |st, args| {
                enrollment_tokens::mint_for_person(
                    st,
                    args,
                    tilo_identity_host::enrollment::EnrollmentRole::HomePeer,
                )
            },
        },
        Action {
            id: device_pairing::DISCOVER_ID,
            label: "search-action-identity-device-pair-discover",
            subtitle: "search-action-identity-device-pair-discover-subtitle",
            params: device_pairing::DISCOVER_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
        },
        Action {
            id: device_pairing::APPROVE_ID,
            label: "search-action-identity-device-pair-approve",
            subtitle: "search-action-identity-device-pair-approve-subtitle",
            params: device_pairing::APPROVE_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
        },
        Action {
            id: device_pairing::REJECT_ID,
            label: "search-action-identity-device-pair-reject",
            subtitle: "search-action-identity-device-pair-reject-subtitle",
            params: device_pairing::REJECT_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
        },
        Action {
            id: device_pairing::ASKING_ID,
            label: "search-action-identity-device-pair-asking",
            subtitle: "search-action-identity-device-pair-asking-subtitle",
            params: device_pairing::ASKING_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
        },
        Action {
            id: device_pairing::ANSWER_ID,
            label: "search-action-identity-device-pair-answer",
            subtitle: "search-action-identity-device-pair-answer-subtitle",
            params: device_pairing::ANSWER_PARAMS,
            apply: |_, _| Err(pairing_needs_lifecycle()),
        },
        Action {
            id: identity_admin::LIST_ID,
            label: "search-action-identity-list",
            subtitle: "search-action-identity-list-subtitle",
            params: crate::shell::identity_admin::LIST_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
        },
        Action {
            id: identity_admin::ACTIVATE_ID,
            label: "search-action-identity-activate",
            subtitle: "search-action-identity-activate-subtitle",
            params: crate::shell::identity_admin::ACTIVATE_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
        },
        Action {
            id: identity_admin::REMOVE_ID,
            label: "search-action-identity-remove",
            subtitle: "search-action-identity-remove-subtitle",
            params: crate::shell::identity_admin::REMOVE_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
        },
        Action {
            id: identity_admin::PROTECT_ID,
            label: "search-action-identity-protect",
            subtitle: "search-action-identity-protect-subtitle",
            params: crate::shell::identity_admin::PROTECT_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
        },
        Action {
            id: identity_admin::DEVICES_ID,
            label: "search-action-identity-devices-list",
            subtitle: "search-action-identity-devices-list-subtitle",
            params: crate::shell::identity_admin::DEVICES_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
        },
        Action {
            id: identity_admin::HOME_PEER_REMOVE_ID,
            label: "search-action-identity-home-peer-remove",
            subtitle: "search-action-identity-home-peer-remove-subtitle",
            params: crate::shell::identity_admin::HOME_PEER_REMOVE_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
        },
        Action {
            id: identity_admin::HOME_PEER_RETRY_ID,
            label: "search-action-identity-home-peer-retry",
            subtitle: "search-action-identity-home-peer-retry-subtitle",
            params: crate::shell::identity_admin::HOME_PEER_RETRY_PARAMS,
            apply: |_, _| Err(identity_needs_lifecycle()),
        },
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
                let store = crate::shell::identity_admin::Store::from_env()
                    .map_err(ActionError::Refused)?;
                crate::shell::identity_admin::revoke(&store, &profile, args)
                    .map_err(ActionError::Refused)?;
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
                st.open_quick_settings_section(crate::shell::quick_settings::QsSection::Clipboard);
                Ok(())
            },
        ),
        chrome(
            "quick-settings.network",
            "qs-network-details",
            "search-action-quick-settings-network-subtitle",
            |st, _| {
                st.open_quick_settings_section(crate::shell::quick_settings::QsSection::Wifi);
                Ok(())
            },
        ),
        chrome(
            "quick-settings.bluetooth",
            "qs-bluetooth-details",
            "search-action-quick-settings-bluetooth-subtitle",
            |st, _| {
                st.open_quick_settings_section(crate::shell::quick_settings::QsSection::Bluetooth);
                Ok(())
            },
        ),
        chrome(
            "quick-settings.devices",
            "search-action-quick-settings-devices",
            "search-action-quick-settings-devices-subtitle",
            |st, _| {
                st.open_quick_settings_section(crate::shell::quick_settings::QsSection::Devices);
                Ok(())
            },
        ),
        chrome(
            "projection.open",
            "search-action-projection-open",
            "search-action-projection-open-subtitle",
            |st, _| {
                st.open_projection();
                Ok(())
            },
        ),
        chrome(
            "projection.close",
            "search-action-projection-close",
            "search-action-projection-close-subtitle",
            |st, _| {
                st.close_projection();
                Ok(())
            },
        ),
        chrome(
            shared_display_actions::INSPECT_ID,
            "search-action-shared-display-inspect",
            "search-action-shared-display-inspect-subtitle",
            |_, _| Ok(()),
        ),
        Action {
            id: shared_display_actions::INCLUDE_ID,
            label: "search-action-shared-display-include",
            subtitle: "search-action-shared-display-include-subtitle",
            params: shared_display_actions::INCLUDE_PARAMS,
            apply: |state, args| {
                let revision = args
                    .integer("revision")
                    .and_then(|value| u64::try_from(value).ok())
                    .ok_or_else(|| ActionError::Explained(t("display-remote-stale")))?;
                shared_display_actions::include(
                    state,
                    args.text("device").unwrap_or_default(),
                    args.text("output").unwrap_or_default(),
                    revision,
                    args.boolean("included").unwrap_or(false),
                )
            },
        },
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
            DISPLAY_OUTPUTS_INSPECT_ID,
            "search-action-display-outputs-inspect",
            "search-action-display-outputs-inspect-subtitle",
            |_, _| Ok(()),
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
        Action {
            id: "presence.wave",
            label: "search-action-presence-wave",
            subtitle: "search-action-presence-wave-subtitle",
            params: PRESENCE_FOLLOW_PARAMS,
            apply: |st, args| {
                let id = args.text("collaborator").unwrap_or_default().trim();
                let here = tilo_desktop::collaborators::collaborator_roster_rows(
                    &st.tree().desktop,
                    &crate::shell::state::local_user_id(),
                )
                .into_iter()
                .any(|row| row.id == id && !row.invited);
                if !here {
                    return Err(ActionError::Refused(format!(
                        "`{id}` is not here in this workspace"
                    )));
                }
                st.push_presence_request(crate::shell::chrome_state::PresenceRequest::Wave(
                    id.to_string(),
                ));
                Ok(())
            },
        },
        Action {
            id: "presence.availability.set",
            label: "search-action-presence-availability-set",
            subtitle: "search-action-presence-availability-set-subtitle",
            params: PRESENCE_AVAILABILITY_PARAMS,
            apply: |st, args| {
                let chosen = match args
                    .text("availability")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "available" => ui::Availability::Available,
                    "focusing" => ui::Availability::Focusing,
                    "away" => ui::Availability::Away,
                    _ => {
                        return Err(ActionError::Refused(
                            "availability must be available, focusing, or away".into(),
                        ));
                    }
                };
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::SetAvailability(chosen),
                );
                Ok(())
            },
        },
        Action {
            id: "presence.visibility.set",
            label: "search-action-presence-visibility-set",
            subtitle: "search-action-presence-visibility-set-subtitle",
            params: PRESENCE_VISIBILITY_PARAMS,
            apply: |st, args| {
                let choice = match args
                    .text("visibility")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "where" => ui::PresenceVisibility::WhereIAm,
                    "here" => ui::PresenceVisibility::ThatImHere,
                    "nothing" => ui::PresenceVisibility::Nothing,
                    _ => {
                        return Err(ActionError::Refused(
                            "visibility must be where, here, or nothing".into(),
                        ));
                    }
                };
                let Some(room) = st
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(crate::shell::state::Workspace::room)
                else {
                    return Err(ActionError::Refused("no workspace is open".into()));
                };
                st.push_presence_request(
                    crate::shell::chrome_state::PresenceRequest::SetVisibility(room, choice),
                );
                Ok(())
            },
        },
        chrome(
            "room.activity.open",
            "search-action-room-activity-open",
            "search-action-room-activity-open-subtitle",
            |st, _| {
                st.open_room_activity(None);
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
        // "Join a workspace…" opens the directory of public workspaces,
        // where a link or a code joins too.
        chrome(
            "peer.connect",
            "search-action-peer-connect",
            "search-action-peer-connect-subtitle",
            |st, _| {
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::BrowseDirectory,
                );
                Ok(())
            },
        ),
        chrome(
            "peer.pair-device",
            "search-action-pair-device",
            "search-action-pair-device-subtitle",
            |st, _| {
                st.push_peer_sync_request(crate::shell::overlay_host::PeerSyncAction::PairDevice);
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
        // Public workspaces: the active one listed by its host with a
        // description and a default role, or private again; one joined
        // from a directory reported to whoever runs it.
        Action {
            id: WORKSPACE_PUBLISH_ID,
            label: "search-action-workspace-publish",
            subtitle: "search-action-workspace-publish-subtitle",
            params: WORKSPACE_PUBLISH_PARAMS,
            apply: |st, args| {
                let listing = publish_listing(st, args)?;
                st.publish_active_workspace(Some(listing));
                st.bump();
                Ok(())
            },
        },
        Action {
            id: WORKSPACE_UNPUBLISH_ID,
            label: "search-action-workspace-unpublish",
            subtitle: "search-action-workspace-unpublish-subtitle",
            params: &[],
            apply: |st, _| {
                st.publish_active_workspace(None);
                st.bump();
                Ok(())
            },
        },
        // Bots of the active workspace: asked of its host, which runs
        // what the room's governance lets this person ask for.
        Action {
            id: BOT_ADD_ID,
            label: "search-action-bot-add",
            subtitle: "search-action-bot-add-subtitle",
            params: BOT_ADD_PARAMS,
            apply: |st, args| {
                require_explicit_confirmation(args, "add this bot")?;
                let ask = bot_ask(args, |entry, version| crate::shell::network::BotAsk::Add {
                    entry,
                    version: version.unwrap_or_default(),
                })?;
                if !st.ask_active_workspace_for_bot(ask) {
                    return Err(ActionError::Refused("no active workspace".into()));
                }
                st.bump();
                Ok(())
            },
        },
        Action {
            id: APP_UNINSTALL_ID,
            label: "search-action-app-uninstall",
            subtitle: "search-action-app-uninstall-subtitle",
            params: APP_UNINSTALL_PARAMS,
            apply: |_, args| uninstall_app(args, false),
        },
        Action {
            id: APP_REMOVE_FROM_APPS_ID,
            label: "search-action-app-remove-from-apps",
            subtitle: "search-action-app-remove-from-apps-subtitle",
            params: APP_REMOVE_FROM_APPS_PARAMS,
            apply: |_, args| uninstall_app(args, true),
        },
        Action {
            id: BOT_REMOVE_ID,
            label: "search-action-bot-remove",
            subtitle: "search-action-bot-remove-subtitle",
            params: BOT_REMOVE_PARAMS,
            apply: |st, args| {
                require_explicit_confirmation(args, "remove this bot")?;
                let ask = bot_ask(args, |entry, _| crate::shell::network::BotAsk::Remove {
                    entry,
                })?;
                if !st.ask_active_workspace_for_bot(ask) {
                    return Err(ActionError::Refused("no active workspace".into()));
                }
                st.bump();
                Ok(())
            },
        },
        Action {
            id: WORKSPACE_REPORT_ID,
            label: "search-action-workspace-report",
            subtitle: "search-action-workspace-report-subtitle",
            params: WORKSPACE_REPORT_PARAMS,
            apply: |st, args| {
                let reason = args.text("reason").unwrap_or_default().trim().to_owned();
                if reason.is_empty() {
                    return Err(ActionError::Refused("a report says what is wrong".into()));
                }
                let room = st
                    .tree()
                    .desktop
                    .active_workspace()
                    .map(crate::shell::state::Workspace::room)
                    .ok_or_else(|| ActionError::Refused("no active workspace".into()))?;
                st.push_peer_sync_request(
                    crate::shell::overlay_host::PeerSyncAction::ReportWorkspace { room, reason },
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
                        .map(|w| (w.id, w.room().to_string(), w.policy.grants.clone()))
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
        // The Agents tab's Remove, for every caller: the same function, so
        // a person clicking and a script or screen reader end an agent's
        // access the same way.
        Action {
            id: crate::shell::membership::AGENT_REMOVE_ACTION,
            label: "search-action-agent-remove",
            subtitle: "search-action-agent-remove-subtitle",
            params: AGENT_REMOVE_PARAMS,
            apply: |st, args| {
                // Case-folded like `agent.invite`, which admitted it.
                let agent = args.text("name").unwrap_or_default().trim().to_lowercase();
                if agent.is_empty() {
                    return Err(ActionError::Refused(
                        "name the agent to remove; nothing was changed".into(),
                    ));
                }
                crate::shell::membership::remove_agent(st, &agent).map_err(Into::into)
            },
        },
        chrome(
            "tab.new.terminal.here",
            "search-action-tab-new-terminal-here",
            "search-action-tab-new-terminal-here-subtitle",
            |st, _| {
                crate::shell::window_actions::new_terminal(
                    st,
                    crate::shell::window_actions::Destination::Here,
                )
                .map_err(|error| ActionError::Refused(error.to_string()))
            },
        ),
        chrome(
            "window.new.terminal",
            "search-action-window-new-terminal",
            "search-action-window-new-terminal-subtitle",
            |st, _| {
                crate::shell::window_actions::new_terminal(
                    st,
                    crate::shell::window_actions::Destination::Beside,
                )
                .map_err(|error| ActionError::Refused(error.to_string()))
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
                    return Err(ActionError::Explained(t(
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
                    return Err(ActionError::Explained(t(
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
        Action {
            id: "tab.rename",
            label: "search-action-tab-rename",
            subtitle: "search-action-tab-rename-subtitle",
            params: TAB_RENAME_PARAMS,
            apply: rename_focused_tab,
        },
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
        Action {
            id: "strip.create",
            label: "search-action-strip-create",
            subtitle: "search-action-strip-create-subtitle",
            params: STRIP_CREATE_PARAMS,
            apply: create_strip,
        },
        Action {
            id: "strip.rename",
            label: "search-action-strip-rename",
            subtitle: "search-action-strip-rename-subtitle",
            params: STRIP_RENAME_PARAMS,
            apply: rename_strip,
        },
        Action {
            id: "strip.move",
            label: "search-action-strip-move",
            subtitle: "search-action-strip-move-subtitle",
            params: STRIP_MOVE_PARAMS,
            apply: move_strip,
        },
        Action {
            id: "strip.remove",
            label: "search-action-strip-remove",
            subtitle: "search-action-strip-remove-subtitle",
            params: STRIP_REMOVE_PARAMS,
            apply: remove_strip,
        },
        Action {
            id: "workspace.create",
            label: "search-action-workspace-create",
            subtitle: "search-action-workspace-create-subtitle",
            params: WORKSPACE_CREATE_PARAMS,
            apply: create_workspace,
        },
        Action {
            id: "workspace.rename",
            label: "search-action-workspace-rename",
            subtitle: "search-action-workspace-rename-subtitle",
            params: WORKSPACE_RENAME_PARAMS,
            apply: rename_workspace,
        },
        Action {
            id: "workspace.delete",
            label: "search-action-workspace-delete",
            subtitle: "search-action-workspace-delete-subtitle",
            params: WORKSPACE_DELETE_PARAMS,
            apply: delete_workspace,
        },
        chrome(
            "workspace.move-earlier",
            "search-action-workspace-move-earlier",
            "search-action-workspace-move-earlier-subtitle",
            |st, _| move_workspace(st, -1),
        ),
        chrome(
            "workspace.move-later",
            "search-action-workspace-move-later",
            "search-action-workspace-move-later-subtitle",
            |st, _| move_workspace(st, 1),
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
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupLeft),
        ),
        chrome(
            "group.move-right",
            "search-action-group-move-right",
            "search-action-group-move-right-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupRight),
        ),
        chrome(
            "group.move-to-strip-previous",
            "search-action-group-move-to-strip-previous",
            "search-action-group-move-to-strip-previous-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupToPreviousStrip),
        ),
        chrome(
            "group.move-to-strip-next",
            "search-action-group-move-to-strip-next",
            "search-action-group-move-to-strip-next-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupToNextStrip),
        ),
        Action {
            id: GROUP_MOVE_TO_STRIP_ID,
            label: "search-action-group-move-to-strip",
            subtitle: "search-action-group-move-to-strip-subtitle",
            params: GROUP_MOVE_TO_STRIP_PARAMS,
            apply: move_group_to_strip,
        },
        chrome(
            "group.close",
            "search-action-group-close",
            "search-action-group-close-subtitle",
            |st, _| {
                use crate::shell::window_actions::{TabActionError, close_active_group};
                close_active_group(st).map_err(|err| match err {
                    TabActionError::NoActiveTab => no_window(),
                    other => ActionError::Explained(other.user_message()),
                })
            },
        ),
        chrome(
            "group.move-to-display-previous",
            "search-action-group-move-to-display-previous",
            "search-action-group-move-to-display-previous-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupToPreviousDisplay),
        ),
        chrome(
            "group.move-to-display-next",
            "search-action-group-move-to-display-next",
            "search-action-group-move-to-display-next-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::MoveGroupToNextDisplay),
        ),
        chrome(
            "group.maximize",
            "search-action-group-maximize",
            "search-action-group-maximize-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::MaximizeWindow),
        ),
        chrome(
            "group.restore",
            "search-action-group-restore",
            "search-action-group-restore-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::RestoreWindow),
        ),
        chrome(
            "group.fullscreen",
            "search-action-group-fullscreen",
            "search-action-group-fullscreen-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::ToggleFullscreen),
        ),
        chrome(
            "group.narrower",
            "search-action-group-narrower",
            "search-action-group-narrower-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::ResizeNarrower),
        ),
        chrome(
            "group.wider",
            "search-action-group-wider",
            "search-action-group-wider-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::ResizeWider),
        ),
        chrome(
            "tab.move-to-group-previous",
            "search-action-tab-move-to-group-previous",
            "search-action-tab-move-to-group-previous-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::AttachTabLeft),
        ),
        chrome(
            "tab.move-to-group-next",
            "search-action-tab-move-to-group-next",
            "search-action-tab-move-to-group-next-subtitle",
            |st, _| request_group_shortcut(st, ShortcutAction::AttachTabRight),
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
        Action {
            id: "strip.focus-nth",
            label: "search-action-strip-focus-nth",
            subtitle: "search-action-strip-focus-nth-subtitle",
            params: STRIP_FOCUS_NTH_PARAMS,
            apply: |st, args| {
                let action = args
                    .integer("index")
                    .and_then(|index| usize::try_from(index).ok()?.checked_sub(1))
                    .and_then(|index| {
                        ShortcutAction::ALL
                            .into_iter()
                            .find(|action| action.focus_strip_index() == Some(index))
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
        Action {
            id: "tab.move-to-workspace",
            label: "search-action-tab-move-to-workspace",
            subtitle: "search-action-tab-move-to-workspace-subtitle",
            params: TAB_MOVE_TO_WORKSPACE_PARAMS,
            apply: move_focused_tab_to_workspace,
        },
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
            "notifications.open",
            "search-action-notifications-open",
            "search-action-notifications-open-subtitle",
            |st, _| {
                st.open_quick_settings_level(crate::shell::quick_settings::QsLevel::Notifications);
                Ok(())
            },
        ),
        chrome(
            "notifications.clear",
            "search-action-notifications-clear",
            "search-action-notifications-clear-subtitle",
            |st, _| {
                if st.notification_log().history().is_empty() {
                    return Err(ActionError::Refused(
                        "there are no notifications to clear".into(),
                    ));
                }
                st.clear_notification_history();
                Ok(())
            },
        ),
        chrome(
            "notifications.clear.undo",
            "search-action-notifications-clear-undo",
            "search-action-notifications-clear-undo-subtitle",
            |st, _| {
                if st.undo_notification_history_clear() {
                    Ok(())
                } else {
                    Err(ActionError::Refused(
                        "no cleared notifications can be brought back".into(),
                    ))
                }
            },
        ),
        chrome(
            "appearance.theme.dark",
            "search-action-appearance-theme-dark",
            "search-action-appearance-theme-dark-subtitle",
            |st, _| {
                let invoked_by = settings_store::InvokingAction::action("appearance.theme.dark");
                set_mode(st, "dark", &invoked_by)
            },
        ),
        chrome(
            "appearance.theme.light",
            "search-action-appearance-theme-light",
            "search-action-appearance-theme-light-subtitle",
            |st, _| {
                let invoked_by = settings_store::InvokingAction::action("appearance.theme.light");
                set_mode(st, "light", &invoked_by)
            },
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
                let user = st.local_user_for_room(&workspace.room());
                let actor = workspace.actor_for_person(&user);
                let target = (workspace.id, workspace.room(), actor);
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

pub(crate) const DISPLAY_EXTEND_ID: &str = "display.layout.extend";
pub(crate) const DISPLAY_MIRROR_ID: &str = "display.layout.mirror";
pub(crate) const DISPLAY_BUILTIN_ONLY_ID: &str = "display.layout.builtin-only";
pub(crate) const DISPLAY_EXTERNAL_ONLY_ID: &str = "display.layout.external-only";
pub(crate) const DISPLAY_KEEP_ID: &str = "display.change.keep";
pub(crate) const DISPLAY_REVERT_ID: &str = "display.change.revert";
pub(crate) const DISPLAY_OUTPUTS_INSPECT_ID: &str = "display.outputs.inspect";
pub(crate) const BLUETOOTH_PAIRING_DECLINE_ID: &str = "bluetooth.pairing.decline";

/// Whether a display change waits for Keep or Revert.
fn display_change_waiting() -> bool {
    use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};
    DisplayTransaction::load().is_some_and(|transaction| {
        matches!(
            transaction.state,
            DisplayTransactionState::Requested
                | DisplayTransactionState::AwaitingConfirmation
                | DisplayTransactionState::ConfirmRequested
        )
    })
}

/// Whether the action `id` has something to act on now. Most always do;
/// these act on something that comes and goes. Each reads state the
/// session publishes as it changes, never storage or a device.
fn action_available(id: &str, chrome: &ChromeState, display_waiting: bool) -> bool {
    match id {
        "projection.close" => chrome.overlays.projection,
        "presence.follow.stop" => chrome.search_context.following,
        DISPLAY_KEEP_ID | DISPLAY_REVERT_ID => display_waiting,
        "dictation.finish" | "dictation.cancel" => chrome.search_context.dictating,
        "notifications.clear" => !chrome.notification_log().history().is_empty(),
        "notifications.clear.undo" => chrome.can_undo_notification_history_clear(),
        BLUETOOTH_PAIRING_DECLINE_ID => crate::bluetooth::PairingPromptHandle::global()
            .current()
            .is_some(),
        _ => true,
    }
}

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

/// The attached displays as the compositor drives them, for an agent or a
/// screen reader to read before choosing a layout: each one's scale, mode,
/// place on the desktop and whether it is primary, the layout they form, and
/// the change waiting for Keep or Revert, if one is. Displays come from what
/// the compositor published in memory; the layout from settings.
fn inspect_display_outputs() -> Result<serde_json::Value, ActionError> {
    use settings_store::hardware::{
        DisplayConfig, DisplayHardwareSnapshot, DisplayTransaction, DisplayTransactionState,
    };
    let snapshot = DisplayHardwareSnapshot::published()
        .ok_or_else(|| ActionError::Refused("the compositor has not published displays".into()))?;
    let set = settings_store::display_mode::attached_set(&snapshot.outputs);
    let committed = DisplayConfig::load_for(&set);
    let waiting = DisplayTransaction::load().filter(|transaction| {
        transaction.set.key == set.key
            && matches!(
                transaction.state,
                DisplayTransactionState::Requested
                    | DisplayTransactionState::AwaitingConfirmation
                    | DisplayTransactionState::ConfirmRequested
            )
    });
    let effective = waiting
        .as_ref()
        .map_or(&committed, |transaction| &transaction.candidate);
    let outputs = snapshot
        .outputs
        .iter()
        .map(|output| {
            let connector = &output.descriptor.connector;
            let config = effective
                .output_for(connector)
                .cloned()
                .unwrap_or_else(|| output.unconfigured());
            let placement = snapshot
                .layout
                .iter()
                .find(|placement| &placement.connector == connector);
            serde_json::json!({
                "output": connector,
                "label": output.descriptor.connector_label,
                "builtin": output.builtin,
                "enabled": config.enabled,
                "primary": config.primary,
                "scale": output.scale,
                "mode": output.current_mode,
                "modes": output.descriptor.modes,
                "mirror_of": config.mirror_of,
                "rect": placement.map(|placement| serde_json::json!({
                    "x": placement.x,
                    "y": placement.y,
                    "width": placement.width,
                    "height": placement.height,
                })),
            })
        })
        .collect::<Vec<_>>();
    let layout = settings_store::display_mode::active_layout(&snapshot.outputs, effective)
        .map(|mode| format!("display.layout.{}", mode.id()));
    Ok(serde_json::json!({
        "outputs": outputs,
        "layout": layout,
        "waiting": waiting.map(|transaction| serde_json::json!({
            "transaction": transaction.id,
            "state": transaction.state,
            "deadline_unix_ms": transaction.deadline_unix_ms,
            "keep": DISPLAY_KEEP_ID,
            "revert": DISPLAY_REVERT_ID,
        })),
    }))
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
        ..tilo_actions::ActionContract::default()
    }
}

fn quick_settings_contract() -> tilo_actions::ActionContract {
    tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        retry: tilo_actions::RetryPolicy::Idempotent,
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
    /// Whether a display change waits for Keep or Revert, as the settings
    /// document the catalogue was built from says: the catalogue is rebuilt
    /// when that document changes, so this is never read per keystroke.
    display_waiting: bool,
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
    /// What the rows that show a setting's state say: Wi-Fi, Bluetooth,
    /// airplane mode, Do Not Disturb, axis navigation.
    states: [bool; 5],
    /// The settings document the rows read: the appearance mode and high
    /// contrast their badges show, and the shortcuts and keyboard layout
    /// their keycaps spell.
    settings: Option<settings_store::SettingsStamp>,
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

/// The provider id the shell's own actions are addressed by.
pub(crate) const PROVIDER_ID: &str = "actions";

/// The id of the action selecting the workspace `id` names in its form from
/// before action ids named the room (`workspace.select.<number>`): that
/// number is a workspace's id here, or one a workspace carried before
/// ([`crate::shell::state::WorkspaceTree::resolve_workspace`]). `None` for
/// any other id.
fn current_workspace_select(
    id: &str,
    chrome: &crate::shell::chrome_state::ChromeState,
) -> Option<String> {
    let number: u64 = id.strip_prefix("workspace.select.")?.parse().ok()?;
    let room = {
        let tree = chrome.tree();
        tree.resolve_workspace(crate::shell::state::WorkspaceId(number))
            .and_then(|workspace| tree.room_of(workspace))
    }?;
    Some(format!("workspace.select.{room}"))
}

impl SearchProvider for ActionsProvider {
    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        action_icon(&result.id)
    }
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Actions"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn toggles_in_place(&self, result: &SearchResult) -> bool {
        toggles_in_place(&result.id)
    }
    fn adjustable(&self, result: &SearchResult) -> bool {
        matches!(result.id.as_str(), QS_VOLUME_ID | QS_BRIGHTNESS_ID)
            || display_brightness_connector(&result.id).is_some()
    }
    /// ←/→ on Volume or Brightness: the same step "louder" or "dimmer" takes.
    /// On one display's brightness row, that display moves by the same step.
    fn adjust(
        &self,
        result: &SearchResult,
        delta: i8,
        doc: &mut ui::widget::Edits<'_>,
        root: &automerge::ObjId,
        chrome: &Chrome,
    ) -> Result<(), ActionError> {
        if let Some(connector) = display_brightness_connector(&result.id) {
            let level = chrome
                .borrow()
                .quick_settings
                .displays
                .iter()
                .find(|display| display.connector == connector)
                .map(|display| (display.level * 100.0).round())
                .ok_or_else(|| ActionError::Refused(format!("{connector} has no brightness")))?;
            let step = if delta > 0 {
                DISPLAY_BRIGHTNESS_STEP
            } else {
                -DISPLAY_BRIGHTNESS_STEP
            };
            let mut arguments = serde_json::Map::new();
            arguments.insert(
                "percent".into(),
                serde_json::json!(f64::from((level + step).clamp(0.0, 100.0))),
            );
            let args = ActionArgs::bind_json(&result.params, &arguments)
                .map_err(ActionError::Arguments)?;
            return (result.on_open)(doc, root, chrome, &args);
        }
        let step = LEVEL_STEPS
            .iter()
            .find(|step| step.level == result.id && (step.delta > 0.0) == (delta > 0))
            .ok_or_else(|| ActionError::Refused(format!("{} has no level to step", result.id)))?;
        let args = stepped(&result.params, chrome, step.level, step.delta)?;
        (result.on_open)(doc, root, chrome, &args)
    }
    fn match_reason(&self, result: &SearchResult, needle: &str) -> Option<String> {
        let catalogue = self.catalogue.borrow().clone()?;
        let at = *catalogue.by_id.get(&result.id)?;
        crate::shell::search::match_reason_in(needle, result, catalogue.fields.get(at)?)
    }
    /// A radio's set-enabled form is for agents: a person flips the switch
    /// row beside it, which already shows whether it is on. And an action
    /// with nothing to act on (closing a sheet that is not open, declining a
    /// pairing nobody asked for, stopping a follow that is not happening)
    /// only takes a row from one that would do something: it is offered
    /// only when the query names it exactly, where running it says why not.
    fn hidden_from_people(
        &self,
        result: &SearchResult,
        needle: &str,
        chrome: &ChromeState,
    ) -> bool {
        if QS_TOGGLES
            .iter()
            .any(|toggle| toggle.set_enabled == result.id)
        {
            return true;
        }
        let display_waiting = self
            .catalogue
            .borrow()
            .as_ref()
            .is_some_and(|catalogue| catalogue.display_waiting);
        !action_available(&result.id, chrome, display_waiting)
            && tilo_search::fold(needle.trim()) != tilo_search::fold(&result.title)
    }
    /// An address is looked up in the whole table. The shell offers more
    /// actions than one palette page shows, so the first page of an empty
    /// query would leave the later ones unreachable by id.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return None;
        }
        let found = if let Some(action) = Action::ALL.iter().find(|action| action.id == id) {
            // Workspace navigation bumps the chrome revision on each invocation.
            // Rebuilding the whole palette here also rebuilt the shortcut table
            // for every bound row, synchronously on the compositor's loop.
            let invite_offered = matches!(id, "peer.invite" | "peer.invite.named")
                && ctx.chrome.peer_sync_enabled
                && crate::shell::invite_policy::active_invite_route(ctx.chrome)
                    .is_some_and(crate::shell::invite_policy::InviteRoute::issues_here);
            self.static_action(action, ctx, invite_offered)
                .map(|mut row| {
                    if ACTION_SHORTCUTS.iter().any(|(action, _)| *action == id) {
                        row.shortcut =
                            action_shortcut(&settings_store::hardware::ShortcutConfig::load(), id);
                    }
                    row
                })
        } else {
            let catalogue = self.catalogue(&ctx);
            // An action id from before ids named the room selects the
            // workspace that number named, through the device's aliases.
            let current = current_workspace_select(id, ctx.chrome);
            let lookup = current.as_deref().unwrap_or(id);
            match catalogue.by_id.get(lookup) {
                Some(&at) => catalogue.rows.get(at).cloned().map(|mut row| {
                    id.clone_into(&mut row.id);
                    row
                }),
                None => crate::shell::notification::alarm_action_rows(ctx.chrome)
                    .into_iter()
                    .find(|row| row.id == id),
            }
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
        // Equal matches: what is chosen most comes first, then the action
        // that does it in one step before the one asking for input, and a
        // switch before a view of the same thing, so "wi-fi" finds the
        // switch ahead of the setter it drives and the list of networks.
        crate::shell::search::usage::order_ties_by(&mut out, |row| {
            i32::from(row.params.is_empty())
                .saturating_mul(2)
                .saturating_add(i32::from(toggles_in_place(&row.id)))
        });
        out
    }
}

impl ActionsProvider {
    /// With more than one display that can be dimmed, a brightness row for
    /// each, named after its display, showing its level: ←/→ steps it in
    /// place, Enter asks for a percentage. One display keeps the single
    /// Brightness row.
    fn push_display_brightness_rows(
        &self,
        out: &mut Vec<SearchResult>,
        candidates: &mut ActionCandidates,
        state: &ChromeState,
    ) {
        let displays = &state.quick_settings.displays;
        if displays.len() < 2 || !self.qs_actions.action_available(QS_BRIGHTNESS_ID) {
            return;
        }
        let Some(base) = out.iter().find(|row| row.id == QS_BRIGHTNESS_ID).cloned() else {
            return;
        };
        let subtitle = t("search-brightness-display-subtitle");
        for display in displays {
            let id = format!("{QS_DISPLAY_BRIGHTNESS_PREFIX}{}", display.connector);
            let run = Rc::clone(&base.on_open);
            let params = base.params.clone();
            let connector = display.connector.clone();
            let on_open = Rc::new(
                move |doc: &mut ui::widget::Edits<'_>,
                      root: &automerge::ObjId,
                      chrome: &Chrome,
                      args: &ActionArgs| {
                    let mut arguments = serde_json::Map::new();
                    if let Some(percent) = args.number("percent") {
                        arguments.insert("percent".into(), serde_json::json!(percent));
                    }
                    arguments.insert("display".into(), serde_json::json!(connector));
                    let args = ActionArgs::bind_json(&params, &arguments)
                        .map_err(ActionError::Arguments)?;
                    run(doc, root, chrome, &args)
                },
            ) as Rc<crate::shell::search::OpenFn>;
            let title = t_args(
                "search-brightness-display",
                &[("display", display.name.as_str().into())],
            );
            candidates.record(&title.to_lowercase(), &subtitle.to_lowercase(), &id);
            out.push(SearchResult {
                id,
                title,
                subtitle: Some(subtitle.clone()),
                badge: Some(t_args(
                    "search-level-percent",
                    &[("percent", f64::from((display.level * 100.0).round()).into())],
                )),
                params: base
                    .params
                    .iter()
                    .filter(|param| param.name != "display")
                    .cloned()
                    .collect(),
                on_open,
                on_invoke: None,
                on_begin_invoke: None,
                ..base.clone()
            });
        }
    }

    /// Louder, quieter, brighter, dimmer and mute, each derived from the
    /// level's set row already in `out`, with the live level as its pill.
    fn push_level_steps(
        &self,
        out: &mut Vec<SearchResult>,
        candidates: &mut ActionCandidates,
        state: &ChromeState,
    ) {
        for step in LEVEL_STEPS {
            let Some(base) = out.iter().find(|row| row.id == step.level).cloned() else {
                continue;
            };
            if !self.qs_actions.action_available(step.level) {
                continue;
            }
            let (level, delta, params) = (step.level, step.delta, base.params.clone());
            let run = Rc::clone(&base.on_open);
            let on_open = Rc::new(
                move |doc: &mut ui::widget::Edits<'_>,
                      root: &automerge::ObjId,
                      chrome: &Chrome,
                      _args: &ActionArgs| {
                    run(doc, root, chrome, &stepped(&params, chrome, level, delta)?)
                },
            ) as Rc<crate::shell::search::OpenFn>;
            let title = t(step.title);
            candidates.record(&title.to_lowercase(), "", step.id);
            out.push(SearchResult {
                id: step.id.to_string(),
                title,
                subtitle: None,
                badge: Some(level_badge(state, level)),
                params: Vec::new(),
                on_open,
                on_invoke: None,
                on_begin_invoke: None,
                ..base
            });
        }
        if let Some(base) = out.iter().find(|row| row.id == QS_VOLUME_ID).cloned()
            && self.qs_actions.action_available(QS_VOLUME_ID)
        {
            let actions = Rc::clone(&self.qs_actions);
            let title = t("search-step-volume-mute");
            candidates.record(&title.to_lowercase(), "", QS_MUTE_ID);
            out.push(SearchResult {
                id: QS_MUTE_ID.to_string(),
                title,
                subtitle: None,
                badge: None,
                params: Vec::new(),
                on_open: Rc::new(
                    move |_doc: &mut ui::widget::Edits<'_>,
                          _root: &automerge::ObjId,
                          chrome: &Chrome,
                          _args: &ActionArgs| {
                        if !current_shell_action_authorized(&chrome.borrow(), QS_MUTE_ID) {
                            return Err(ActionError::Refused(format!(
                                "current workspace authority no longer permits {QS_MUTE_ID}"
                            )));
                        }
                        let backend = actions.actions.borrow().clone();
                        backend.toggle_mute().map_err(ActionError::Refused)
                    },
                ),
                on_invoke: None,
                on_begin_invoke: None,
                ..base
            });
        }
    }

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
            states: [
                ctx.chrome.quick_settings.wifi,
                ctx.chrome.quick_settings.bluetooth,
                ctx.chrome.quick_settings.radios.airplane_mode,
                ctx.chrome.dnd(),
                ctx.chrome.axis_navigation_enabled(),
            ],
            settings: settings_store::stamp(),
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

    /// Build one table action, shared by palette enumeration and exact invocation.
    /// Exact addresses must not build unrelated rows after every chrome mutation.
    #[expect(
        clippy::too_many_lines,
        reason = "the table action's synchronous and lifecycle callbacks share one construction path"
    )]
    fn static_action(
        &self,
        action: &Action,
        ctx: SearchContext<'_>,
        invite_offered: bool,
    ) -> Option<SearchResult> {
        // A session not running peer sync has nothing to invite into.
        if (action.id.starts_with("peer.") || INVITATION_ACTION_IDS.contains(&action.id))
            && !ctx.chrome.peer_sync_enabled
        {
            return None;
        }
        // The one invitation policy: no row for an invitation the issuer
        // would refuse from here, as there is no composer for it.
        if matches!(action.id, "peer.invite" | "peer.invite.named") && !invite_offered {
            return None;
        }
        // Needs a room to record the capability in and a working
        // broker (peer sync is not required — an agent uses this
        // session's own broker socket).
        if action.id.starts_with("agent.")
            && (!ctx.chrome.collab_available()
                || ctx.chrome.tree().desktop.active_workspace().is_none())
        {
            return None;
        }
        let label = t(action.label);
        let subtitle = t(action.subtitle);
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
                            let user = state.local_user_for_room(&workspace.room());
                            let actor = workspace.actor_for_person(&user);
                            if !shell_action_authorized_for(workspace, &actor, "record.bring") {
                                return Err(ActionError::Refused(
                                    "current workspace authority no longer permits record.bring"
                                        .into(),
                                ));
                            }
                            let located = (workspace.id, workspace.room(), actor);
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
                        let ticket: crate::shell::search::PendingInvocation = Rc::new(move || {
                            match chrome.borrow_mut().take_record_request_result(request_id) {
                                None => Ok(None),
                                Some(Ok(())) => {
                                    Ok(Some(crate::shell::search::ActionReceipt::completed()))
                                }
                                Some(Err(error)) => Err(ActionError::Refused(error)),
                            }
                        });
                        Ok(crate::shell::search::InvocationDisposition::Pending(ticket))
                    },
                ) as Rc<crate::shell::search::BeginInvokeFn>;
                (on_open, Some(begin))
            } else if matches!(
                action.id,
                "peer.invite" | "peer.connect" | "peer.invite.named" | "peer.pair-device"
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
                            if !matches!(action_id, "peer.connect" | "peer.pair-device")
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
                                    crate::shell::overlay_host::PeerSyncAction::BrowseDirectory
                                }
                                "peer.pair-device" => {
                                    crate::shell::overlay_host::PeerSyncAction::PairDevice
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
            } else if device_pairing::is_pair_action(action.id) {
                let action_id = action.id;
                let on_open = Rc::new(
                    |_doc: &mut ui::widget::Edits<'_>,
                     _root: &automerge::ObjId,
                     _chrome: &Chrome,
                     _args: &ActionArgs| { Err(pairing_needs_lifecycle()) },
                ) as Rc<crate::shell::search::OpenFn>;
                let begin = Rc::new(
                    move |_doc: &mut ui::widget::Edits<'_>,
                          _root: &automerge::ObjId,
                          chrome: &Chrome,
                          args: &ActionArgs| {
                        device_pairing::begin(
                            crate::shell::device_pairing::Pairing::session(),
                            chrome,
                            action_id,
                            args,
                        )
                    },
                ) as Rc<crate::shell::search::BeginInvokeFn>;
                (on_open, Some(begin))
            } else if identity_admin::is_identity_action(action.id) {
                let action_id = action.id;
                let on_open = Rc::new(
                    |_doc: &mut ui::widget::Edits<'_>,
                     _root: &automerge::ObjId,
                     _chrome: &Chrome,
                     _args: &ActionArgs| { Err(identity_needs_lifecycle()) },
                ) as Rc<crate::shell::search::OpenFn>;
                let begin = Rc::new(
                    move |_doc: &mut ui::widget::Edits<'_>,
                          _root: &automerge::ObjId,
                          chrome: &Chrome,
                          args: &ActionArgs| {
                        identity_admin::begin(
                            crate::shell::identity_admin::IdentityAdmin::session(),
                            chrome,
                            action_id,
                            args,
                        )
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
                            return Err(ActionError::Refused(
                                "current workspace authority no longer permits this display layout"
                                    .into(),
                            ));
                        }
                        let pending = crate::shell::display_persistence::stage_layout(mode)
                            .map_err(|error| ActionError::Refused(error.to_string()))?;
                        {
                            let mut state = chrome.borrow_mut();
                            state.open_projection_for_action();
                            state.bump();
                        }
                        let chrome = chrome.clone();
                        // The sheet this opened follows the stored change
                        // and closes once it settles; a refused layout
                        // leaves nothing to answer, so it closes at once.
                        let ticket: crate::shell::search::PendingInvocation =
                            Rc::new(move || match pending.poll() {
                                Ok(done) => Ok(done.map(|transaction| {
                                    let mut state = chrome.borrow_mut();
                                    state.projection_staged(transaction.id);
                                    state.bump();
                                    // The change to answer with Keep or
                                    // Revert, for the caller to follow.
                                    crate::shell::search::ActionReceipt::completed_with(
                                        serde_json::json!({ "transaction": transaction.id }),
                                    )
                                })),
                                Err(error) => {
                                    let mut state = chrome.borrow_mut();
                                    state.projection_refused();
                                    state.bump();
                                    Err(ActionError::Refused(error.to_string()))
                                }
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
                            return Err(ActionError::Refused(
                                "current workspace authority no longer permits this display answer"
                                    .into(),
                            ));
                        }
                        let pending = answer(if action_id == DISPLAY_KEEP_ID {
                            Answer::Keep
                        } else {
                            Answer::Revert
                        })
                        .map_err(|error| ActionError::Refused(error.to_string()))?;
                        let chrome = chrome.clone();
                        let ticket: crate::shell::search::PendingInvocation = Rc::new(move || {
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
                let creation_context = (action_id == "workspace.create")
                    .then(|| workspace_create_context(ctx.chrome).ok());
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
                        if let Some(expected) = &creation_context
                            && workspace_create_context(&st).ok().as_ref() != expected.as_ref()
                        {
                            return Err(ActionError::Refused(
                                "workspace creation context changed; resolve the action again"
                                    .into(),
                            ));
                        }
                        let outcome = apply(&mut st, args);
                        // Repaint only on success; a refusal changed nothing.
                        if outcome.is_ok() {
                            st.bump();
                        }
                        outcome
                    },
                ) as Rc<crate::shell::search::OpenFn>;
                // A rearrangement the overlay host carries out answers once
                // it ran, with whether anything moved.
                let on_begin_invoke = WINDOW_LAYOUT_ACTION_IDS.contains(&action_id).then(|| {
                    let open = Rc::clone(&on_open);
                    Rc::new(
                        move |doc: &mut ui::widget::Edits<'_>,
                              root: &automerge::ObjId,
                              chrome: &Chrome,
                              args: &ActionArgs| {
                            let from = chrome.borrow().window_requests_queued();
                            open(doc, root, chrome, args)?;
                            let outcomes = chrome.borrow_mut().answer_window_requests_since(from);
                            Ok(window_outcome(action_id, outcomes))
                        },
                    ) as Rc<crate::shell::search::BeginInvokeFn>
                });
                (on_open, on_begin_invoke)
            };
            Some(SearchResult {
                facts: Vec::new(),
                shortcut: None,
                kind: ResourceKind::Action,
                operation_kind: shell_action_kind(action.id),
                provider_id: self.id().to_string(),
                id: action.id.to_string(),
                title: label,
                subtitle: Some(subtitle),
                badge: state_badge(action.id, ctx.chrome),
                score,
                params: live_params(action, ctx.chrome),
                contract: Some(shell_action_contract(action.id)),
                on_open,
                on_invoke: match action.id {
                    shared_display_actions::INSPECT_ID => Some(Rc::new(
                        |_: &mut ui::widget::Edits<'_>,
                         _: &automerge::ObjId,
                         _: &Chrome,
                         _: &ActionArgs| {
                            shared_display_actions::inspect()
                                .map(crate::shell::search::ActionReceipt::completed_with)
                        },
                    )
                        as Rc<crate::shell::search::InvokeFn>),
                    DISPLAY_OUTPUTS_INSPECT_ID => Some(Rc::new(
                        |_: &mut ui::widget::Edits<'_>,
                         _: &automerge::ObjId,
                         _: &Chrome,
                         _: &ActionArgs| {
                            inspect_display_outputs()
                                .map(crate::shell::search::ActionReceipt::completed_with)
                        },
                    )
                        as Rc<crate::shell::search::InvokeFn>),
                    // The setup code is the action's answer, for a script
                    // or agent to hand to the joining device.
                    id if enrollment_tokens::role_of(id).is_some() => Some(Rc::new(
                        move |_: &mut ui::widget::Edits<'_>,
                              _: &automerge::ObjId,
                              chrome: &Chrome,
                              args: &ActionArgs| {
                            let role = enrollment_tokens::role_of(id).ok_or_else(|| {
                                ActionError::Refused(format!("{id} mints no setup code"))
                            })?;
                            let state = chrome.borrow();
                            if !current_shell_action_authorized(&state, id) {
                                return Err(ActionError::Refused(format!(
                                    "current workspace authority no longer permits {id}"
                                )));
                            }
                            enrollment_tokens::mint(&state, args, role)
                                .map(crate::shell::search::ActionReceipt::completed_with)
                        },
                    )
                        as Rc<crate::shell::search::InvokeFn>),
                    _ => None,
                },
                on_begin_invoke,
            })
        }
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
            if let Some(row) = self.static_action(action, ctx, key.state.invite_offered) {
                candidates.record(
                    &row.title.to_lowercase(),
                    &row.subtitle.as_deref().unwrap_or_default().to_lowercase(),
                    &row.id,
                );
                out.push(row);
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
                facts: Vec::new(),
                shortcut: None,
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
        // Each archived workspace: bring it back, or delete it with its
        // room, as workspace settings offer.
        let archived: Vec<(crate::shell::state::RoomId, String)> = ctx
            .chrome
            .tree()
            .archived_workspaces
            .iter()
            .map(|workspace| (workspace.room(), workspace.name.clone()))
            .collect();
        for (room, name) in archived {
            for delete in [false, true] {
                let (id, title, subtitle) = if delete {
                    (
                        format!("workspace.archived.delete.{room}"),
                        t_args(
                            "search-action-archived-delete",
                            &[("workspace", name.clone().into())],
                        ),
                        t("search-action-archived-delete-subtitle"),
                    )
                } else {
                    (
                        format!("workspace.archived.restore.{room}"),
                        t_args(
                            "search-action-archived-restore",
                            &[("workspace", name.clone().into())],
                        ),
                        t("search-action-archived-restore-subtitle"),
                    )
                };
                candidates.record(&title.to_lowercase(), &subtitle.to_lowercase(), &id);
                let mut contract = tilo_actions::ActionContract::view();
                if delete {
                    contract.kind = tilo_actions::ActionKind::Outside;
                    contract.risk = tilo_actions::RiskLevel::Destructive;
                    contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
                    contract.retry = tilo_actions::RetryPolicy::AtMostOnce;
                }
                out.push(SearchResult {
                    facts: Vec::new(),
                    shortcut: None,
                    kind: ResourceKind::Action,
                    operation_kind: tilo_actions::OperationKind::View,
                    provider_id: self.id().into(),
                    id,
                    title,
                    subtitle: Some(subtitle),
                    badge: None,
                    score: 0,
                    params: vec![],
                    contract: Some(contract),
                    on_open: Rc::new(move |_, _, chrome, _| {
                        let mut state = chrome.borrow_mut();
                        let done = if delete {
                            state.delete_archived_workspace(room)
                        } else {
                            state.restore_archived_workspace(room)
                        };
                        if done {
                            Ok(())
                        } else {
                            Err(ActionError::Refused(
                                "that archived workspace is not here".into(),
                            ))
                        }
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                });
            }
        }
        let morning = ctx.chrome.tree().active_workspace().map(|workspace| {
            (
                workspace.id,
                workspace.room(),
                crate::shell::room_activity::morning_source_actions(workspace),
            )
        });
        if let Some((workspace, room, actions)) = morning {
            for (id, label) in actions {
                let subtitle = t("activity-morning-description");
                candidates.record(&label.to_lowercase(), &subtitle.to_lowercase(), &id);
                let command = id.starts_with(tilo_desktop::activity::morning::MORNING_REFRESH)
                    || id.starts_with(tilo_desktop::activity::morning::MORNING_DRAFT);
                let kind = if command {
                    tilo_actions::OperationKind::Effect
                } else {
                    tilo_actions::OperationKind::View
                };
                let contract = if command {
                    tilo_actions::ActionContract {
                        kind: tilo_actions::ActionKind::Read,
                        ..Default::default()
                    }
                } else {
                    tilo_actions::ActionContract::view()
                };
                let request = id.clone();
                out.push(SearchResult {
                    facts: Vec::new(),
                    shortcut: None,
                    kind: ResourceKind::Action,
                    operation_kind: kind,
                    provider_id: self.id().into(),
                    id,
                    title: label,
                    subtitle: Some(subtitle),
                    badge: None,
                    score: 0,
                    params: vec![],
                    contract: Some(contract),
                    on_open: Rc::new(move |_, _, chrome, _| {
                        let mut state = chrome.borrow_mut();
                        if !state
                            .tree()
                            .active_workspace()
                            .is_some_and(|active| active.id == workspace && active.room() == room)
                        {
                            return Err(ActionError::Refused("Workspace changed".into()));
                        }
                        state
                            .request_history_selection_action(&request)
                            .map_err(ActionError::Refused)
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                });
            }
        }
        for (id, label_key, subtitle_key, keywords_key) in [
            (
                tilo_desktop::activity::morning::MORNING_SOURCE,
                "activity-morning-choose",
                "activity-morning-description",
                "activity-morning-choose",
            ),
            (
                tilo_desktop::activity::ACTIVITY_ACKNOWLEDGE,
                "search-action-activity-acknowledge",
                "search-action-activity-acknowledge-subtitle",
                "search-action-activity-acknowledge-keywords",
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
                facts: Vec::new(),
                shortcut: None,
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
                facts: Vec::new(),
                shortcut: None,
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
                            state
                                .quick_settings
                                .set_volume_level(validated_percent(args) / 100.0);
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
        // One-step switches for the radios: "wifi" finds the Wi-Fi switch, not a
        // form asking for `enabled`, and the row says what it is now. They run
        // the same set-enabled action with the opposite of the live state.
        for toggle in QS_TOGGLES {
            let (set_enabled_id, toggle_id) = (toggle.set_enabled, toggle.id);
            let Some(base) = out.iter().find(|row| row.id == set_enabled_id).cloned() else {
                continue;
            };
            if !self.qs_actions.action_available(set_enabled_id) {
                continue;
            }
            let on = radio_is_on(ctx.chrome, set_enabled_id);
            // A switch is its noun; the pill says whether it is on.
            let title = t(toggle.name);
            let subtitle = t(toggle.subtitle);
            let params = base.params.clone();
            let opposite = move |chrome: &Chrome| -> Result<ActionArgs, ActionError> {
                let enabled = !radio_is_on(&chrome.borrow(), set_enabled_id);
                let mut arguments = serde_json::Map::new();
                arguments.insert("enabled".into(), serde_json::Value::Bool(enabled));
                ActionArgs::bind_json(&params, &arguments).map_err(ActionError::Arguments)
            };
            let on_open = {
                let run = Rc::clone(&base.on_open);
                let opposite = opposite.clone();
                Rc::new(
                    move |doc: &mut ui::widget::Edits<'_>,
                          root: &automerge::ObjId,
                          chrome: &Chrome,
                          _args: &ActionArgs| {
                        run(doc, root, chrome, &opposite(chrome)?)
                    },
                ) as Rc<crate::shell::search::OpenFn>
            };
            let on_begin_invoke = base.on_begin_invoke.clone().map(|begin| {
                Rc::new(
                    move |doc: &mut ui::widget::Edits<'_>,
                          root: &automerge::ObjId,
                          chrome: &Chrome,
                          _args: &ActionArgs| {
                        begin(doc, root, chrome, &opposite(chrome)?)
                    },
                ) as Rc<crate::shell::search::BeginInvokeFn>
            });
            candidates.record(&title.to_lowercase(), &subtitle.to_lowercase(), toggle_id);
            out.push(SearchResult {
                id: toggle_id.to_string(),
                title,
                subtitle: Some(subtitle),
                badge: Some(on_off_badge(on)),
                params: Vec::new(),
                on_open,
                on_begin_invoke,
                ..base
            });
        }
        for row in &mut out {
            if matches!(row.id.as_str(), QS_VOLUME_ID | QS_BRIGHTNESS_ID) {
                row.badge = Some(level_badge(ctx.chrome, &row.id));
            }
        }
        self.push_level_steps(&mut out, &mut candidates, ctx.chrome);
        self.push_display_brightness_rows(&mut out, &mut candidates, ctx.chrome);
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
                        facts: Vec::new(),
                        shortcut: None,
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
                        facts: Vec::new(),
                        shortcut: None,
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
            facts: Vec::new(),
                    shortcut: None,
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
                        facts: Vec::new(),
                        shortcut: None,
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
        // Each row that has a shortcut shows it, as the keyboard layout in use
        // spells it.
        let shortcuts = settings_store::hardware::ShortcutConfig::load();
        for row in &mut out {
            row.shortcut = action_shortcut(&shortcuts, &row.id);
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
            display_waiting: display_change_waiting(),
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

/// The words people use for the action `id`, in every language the shell
/// has ("dark mode", "dunkelmodus", "night"): what a person types who does
/// not know what the row is called. Missing for an action means it is found
/// by its label alone.
pub(crate) fn action_keywords(id: &str) -> Vec<String> {
    crate::shell::i18n::words_in_every_locale(&format!(
        "{}{}",
        crate::shell::i18n::KEYWORD_PREFIX,
        id.replace(['.', '_', ':'], "-")
    ))
}

/// Actions that change a setting the row itself shows, so choosing one
/// leaves the palette open on the row's new state: a person flipping Wi-Fi
/// or Do Not Disturb wants to see it flip, not lose the palette.
/// The picture of an action, by what it acts on: the most specific family
/// its id names (`appearance.theme.dark` before `appearance`). Families
/// are whole id segments, so `tab` never claims `table.*`.
pub(crate) fn action_icon(id: &str) -> Option<ui::icons::Icon> {
    use ui::icons::Icon;
    const FAMILIES: &[(&str, Icon)] = &[
        ("quick-settings.wifi", Icon::Wifi),
        ("quick-settings.network", Icon::Wifi),
        ("quick-settings.bluetooth", Icon::Bluetooth),
        ("quick-settings.airplane-mode", Icon::Plane),
        ("quick-settings.devices", Icon::MonitorSmartphone),
        ("quick-settings.clipboard", Icon::ClipboardPaste),
        ("quick-settings", Icon::Settings2),
        ("appearance.theme.dark", Icon::Moon),
        ("appearance.theme.light", Icon::Sun),
        ("appearance.language", Icon::Globe),
        ("appearance", Icon::Palette),
        ("notifications", Icon::Bell),
        ("capture", Icon::Camera),
        ("dictation", Icon::Mic),
        ("presence.follow", Icon::Eye),
        ("presence", Icon::User),
        ("peer", Icon::UserPlus),
        ("agent", Icon::Bot),
        ("invitation", Icon::Send),
        ("identity", Icon::Fingerprint),
        ("ownership", Icon::Shield),
        ("collaborators", Icon::Users),
        ("history.undo", Icon::Undo2),
        ("history", Icon::History),
        ("room.activity", Icon::History),
        ("record", Icon::FileText),
        ("tab.close", Icon::Trash2),
        ("group.close", Icon::Trash2),
        ("workspace.delete", Icon::Trash2),
        ("tab.rename", Icon::Pencil),
        ("tab.reopen", Icon::RotateCcw),
        ("tab.split-right", Icon::Columns2),
        ("tab.new.terminal", Icon::SquareTerminal),
        ("window.new.terminal", Icon::SquareTerminal),
        ("group.fullscreen", Icon::Maximize2),
        ("group.maximize", Icon::Maximize2),
        ("tab", Icon::AppWindow),
        ("window", Icon::AppWindow),
        ("group", Icon::AppWindow),
        ("strip", Icon::Layers),
        ("workspace", Icon::Layers),
        ("overview", Icon::LayoutGrid),
        ("projection", Icon::ScreenShare),
        ("navigation", Icon::Navigation),
        ("settings", Icon::Settings),
        ("feedback", Icon::MessageSquare),
        ("desktop-tour", Icon::Info),
        ("shortcut-help", Icon::Keyboard),
        ("topbar", Icon::Menu),
    ];
    FAMILIES
        .iter()
        .find(|(family, _)| {
            id.strip_prefix(family)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
        .map(|(_, icon)| *icon)
}

pub(crate) fn toggles_in_place(id: &str) -> bool {
    matches!(
        id,
        "notifications.dnd.toggle"
            | "appearance.theme.dark"
            | "appearance.theme.light"
            | "navigation.axis.enable"
            | "navigation.axis.disable"
    ) || QS_TOGGLES.iter().any(|toggle| toggle.id == id)
        || LEVEL_STEPS.iter().any(|step| step.id == id)
        || id == QS_MUTE_ID
}

/// A Quick Settings radio offered as a one-step toggle: the set-enabled
/// action it drives, its own id, and the catalogue keys of its row's title
/// and subtitle.
struct QsToggle {
    set_enabled: &'static str,
    id: &'static str,
    /// The switch's name; its state is the row's pill.
    name: &'static str,
    subtitle: &'static str,
}

/// A step up or down a Quick Settings level: "louder", "dimmer". The row
/// runs the level's own set action with the live level moved by `delta`
/// percent, so it is one Enter (or one more) instead of typing a number.
struct LevelStep {
    id: &'static str,
    /// The set action whose level it moves.
    level: &'static str,
    delta: f32,
    title: &'static str,
}

const LEVEL_STEPS: &[LevelStep] = &[
    LevelStep {
        id: "quick-settings.volume.up",
        level: QS_VOLUME_ID,
        delta: 5.0,
        title: "search-step-volume-up",
    },
    LevelStep {
        id: "quick-settings.volume.down",
        level: QS_VOLUME_ID,
        delta: -5.0,
        title: "search-step-volume-down",
    },
    LevelStep {
        id: "quick-settings.brightness.up",
        level: QS_BRIGHTNESS_ID,
        delta: 10.0,
        title: "search-step-brightness-up",
    },
    LevelStep {
        id: "quick-settings.brightness.down",
        level: QS_BRIGHTNESS_ID,
        delta: -10.0,
        title: "search-step-brightness-down",
    },
];

/// Muting is its own row: a step to zero would forget the level.
pub(crate) const QS_MUTE_ID: &str = "quick-settings.volume.mute";

/// One display's brightness row: `quick-settings.brightness.on.DP-2`.
const QS_DISPLAY_BRIGHTNESS_PREFIX: &str = "quick-settings.brightness.on.";

/// How far ←/→ moves one display's brightness row: the brightness keys'
/// step, as "brighter" and "dimmer" take.
const DISPLAY_BRIGHTNESS_STEP: f32 = ui::tokens::Layout::BASE.display_brightness_step;

/// The connector a display's brightness row sets, from its id.
fn display_brightness_connector(id: &str) -> Option<&str> {
    id.strip_prefix(QS_DISPLAY_BRIGHTNESS_PREFIX)
        .filter(|connector| !connector.is_empty())
}

/// The display the person works on, when the desktop spans several and its
/// brightness can be set: what "brighter" and "dimmer" adjust.
fn focused_brightness(
    state: &ChromeState,
) -> Option<&crate::shell::chrome_state::DisplayBrightness> {
    let focused = state.quick_settings.focused_display.as_deref()?;
    state
        .quick_settings
        .displays
        .iter()
        .find(|display| display.connector == focused)
}

/// The live level, in percent, of the Quick Settings set action `level`:
/// for brightness, the level of the display the person works on.
fn level_percent(state: &ChromeState, level: &str) -> f32 {
    let share = if level == QS_VOLUME_ID {
        state.quick_settings.volume_level()
    } else {
        focused_brightness(state).map_or(state.quick_settings.brightness, |display| display.level)
    };
    (share * 100.0).round()
}

/// `"40 %"`: a level as its row's pill says it.
fn level_badge(state: &ChromeState, level: &str) -> String {
    t_args(
        "search-level-percent",
        &[("percent", f64::from(level_percent(state, level)).into())],
    )
}

/// The arguments that set `level` to its live value moved by `delta`.
fn stepped(
    params: &[ActionParam],
    chrome: &Chrome,
    level: &str,
    delta: f32,
) -> Result<ActionArgs, ActionError> {
    let mut arguments = serde_json::Map::new();
    {
        let state = chrome.borrow();
        let percent = (level_percent(&state, level) + delta).clamp(0.0, 100.0);
        arguments.insert("percent".into(), serde_json::json!(f64::from(percent)));
        // "Brighter" acts on the display the person works on.
        if level == QS_BRIGHTNESS_ID
            && let Some(display) = focused_brightness(&state)
        {
            arguments.insert("display".into(), serde_json::json!(display.connector));
        }
    }
    ActionArgs::bind_json(params, &arguments).map_err(ActionError::Arguments)
}

/// The Quick Settings radios offered as one-step toggles.
const QS_TOGGLES: &[QsToggle] = &[
    QsToggle {
        set_enabled: QS_WIFI_ID,
        id: "quick-settings.wifi.toggle",
        name: "search-toggle-wifi",
        subtitle: "search-toggle-wifi-subtitle",
    },
    QsToggle {
        set_enabled: QS_BLUETOOTH_ID,
        id: "quick-settings.bluetooth.toggle",
        name: "search-toggle-bluetooth",
        subtitle: "search-toggle-bluetooth-subtitle",
    },
    QsToggle {
        set_enabled: QS_AIRPLANE_ID,
        id: "quick-settings.airplane-mode.toggle",
        name: "search-toggle-airplane-mode",
        subtitle: "search-toggle-airplane-mode-subtitle",
    },
];

/// Whether the radio `set_enabled_id` drives is on now.
fn radio_is_on(state: &ChromeState, set_enabled_id: &str) -> bool {
    match set_enabled_id {
        QS_WIFI_ID => state.quick_settings.wifi,
        QS_BLUETOOTH_ID => state.quick_settings.bluetooth,
        _ => state.quick_settings.radios.airplane_mode,
    }
}

/// `ON` or `OFF`, as the search-source switches say it.
pub(super) fn on_off_badge(on: bool) -> String {
    t(if on {
        "search-action-provider-badge-on"
    } else {
        "search-action-provider-badge-off"
    })
}

/// What a row shows of the setting it changes, so a toggle reads as a
/// switch: on or off, or which mode is current.
fn state_badge(id: &str, state: &ChromeState) -> Option<String> {
    use settings_store::appearance::{HIGH_CONTRAST, MODE};
    match id {
        "notifications.dnd.toggle" => Some(on_off_badge(state.dnd())),
        "navigation.axis.enable" => state
            .axis_navigation_enabled()
            .then(|| t("search-badge-state-current")),
        "navigation.axis.disable" => {
            (!state.axis_navigation_enabled()).then(|| t("search-badge-state-current"))
        }
        "appearance.theme.dark" | "appearance.theme.light" => {
            let document = settings_store::load();
            let mode = MODE.from_document(&document);
            let wanted = if id.ends_with("dark") {
                "dark"
            } else {
                "light"
            };
            (mode.as_deref() == Some(wanted) && !HIGH_CONTRAST.from_document(&document))
                .then(|| t("search-badge-state-current"))
        }
        _ => None,
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
        let mut row = vec![
            (label.to_owned(), FieldWeight::TITLE),
            (id.to_owned(), ACTION_ID_WEIGHT),
            (subtitle.to_owned(), FieldWeight::SUBTITLE),
        ];
        row.extend(
            action_keywords(id)
                .into_iter()
                .map(|word| (word, FieldWeight::KEYWORD)),
        );
        // "Join a workspace…" is also found by the community site's name
        // ("tilo.coffee"), which is the image's configuration, not a word.
        if id == "peer.connect"
            && let Some(site) = crate::shell::network::configured_community_site()
            && let Some(host) = site.host_str()
        {
            row.push((host.to_owned(), FieldWeight::KEYWORD));
        }
        self.0.push(row);
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
    fn adding_a_bot_asks_the_active_workspace_once_the_person_confirms() {
        use crate::shell::network::BotAsk;
        use crate::shell::overlay_host::PeerSyncAction;
        let chrome = chrome_with_room();
        let room = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .room();
        let standup = serde_json::json!({
            "entry": "so.tilo.bots.standup",
            "version": "1.2.0",
        });
        assert!(
            invoke_shell_action(&chrome, BOT_ADD_ID, &standup).is_err(),
            "nothing is asked of the workspace before the person confirms"
        );
        assert!(chrome.borrow_mut().take_peer_sync_requests().is_empty());
        let mut confirmed = standup;
        confirmed["confirm"] = true.into();
        invoke_shell_action(&chrome, BOT_ADD_ID, &confirmed).unwrap();
        assert_eq!(
            chrome.borrow_mut().take_peer_sync_requests(),
            vec![PeerSyncAction::AskForBot {
                room,
                ask: BotAsk::Add {
                    entry: "so.tilo.bots.standup".into(),
                    version: "1.2.0".into(),
                },
            }]
        );
        assert!(
            invoke_shell_action(
                &chrome,
                BOT_ADD_ID,
                &serde_json::json!({"entry": "../standup", "version": "1.2.0", "confirm": true}),
            )
            .is_err(),
            "a bot is a directory entry, never a path"
        );
        invoke_shell_action(
            &chrome,
            BOT_REMOVE_ID,
            &serde_json::json!({"entry": "so.tilo.bots.standup", "confirm": true}),
        )
        .unwrap();
        assert_eq!(
            chrome.borrow_mut().take_peer_sync_requests(),
            vec![PeerSyncAction::AskForBot {
                room,
                ask: BotAsk::Remove {
                    entry: "so.tilo.bots.standup".into(),
                },
            }]
        );
        // An app may prepare adding a bot for the person, and nothing else.
        assert!(app_offerable_label(BOT_ADD_ID).is_some());
        assert!(app_offerable_label(BOT_REMOVE_ID).is_none());
        assert!(app_offerable_label(WORKSPACE_PUBLISH_ID).is_none());
    }

    #[test]
    fn display_outputs_inspection_reads_the_published_displays_and_the_waiting_change() {
        use settings_store::hardware::{
            DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplayTransaction,
            DisplayTransactionState, LiveOutputPlacement, OutputDescriptor, OutputMode,
        };
        let _home = tilo_test_env::TestHome::new().unwrap();
        let chrome = chrome_with_room();
        assert!(
            invoke_shell_action(&chrome, DISPLAY_OUTPUTS_INSPECT_ID, &serde_json::json!({}))
                .is_err(),
            "with nothing published there is nothing to inspect"
        );
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let output = |connector: &str, builtin: bool| DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: connector.into(),
                modes: vec![mode],
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: mode,
            scale: if builtin { 2.0 } else { 1.0 },
            builtin,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: std::collections::BTreeMap::new(),
        };
        let (publisher_pid, publisher_start_ticks) =
            settings_store::hardware::publisher_identity().unwrap();
        let outputs = vec![output("eDP-1", true), output("HDMI-A-1", false)];
        DisplayHardwareSnapshot {
            publisher_pid,
            publisher_start_ticks,
            outputs: outputs.clone(),
            layout: vec![LiveOutputPlacement {
                connector: "HDMI-A-1".into(),
                x: 960,
                y: 0,
                width: 1920,
                height: 1080,
            }],
            ..DisplayHardwareSnapshot::default()
        }
        .save()
        .unwrap();
        let inspect = |chrome: &Chrome| {
            let Ok(crate::shell::search::InvocationDisposition::Accepted(receipt)) =
                invoke_shell_action(chrome, DISPLAY_OUTPUTS_INSPECT_ID, &serde_json::json!({}))
            else {
                panic!("inspection completes immediately")
            };
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
                panic!("inspection returns the displays")
            };
            output.value.unwrap()
        };
        let value = inspect(&chrome);
        let listed = value["outputs"].as_array().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0]["output"], "eDP-1");
        assert_eq!(listed[0]["builtin"], true);
        assert_eq!(listed[0]["scale"], 2.0);
        assert_eq!(listed[1]["rect"]["x"], 960);
        assert_eq!(listed[1]["mode"]["width"], 1920);
        assert!(value["waiting"].is_null());

        let set = settings_store::display_mode::attached_set(&outputs);
        let candidate = settings_store::display_mode::plan_layout(
            DisplayLayoutMode::Mirror,
            &outputs,
            &settings_store::hardware::DisplayConfig::default(),
        )
        .unwrap();
        let mut staged = DisplayTransaction::request(set, candidate).unwrap();
        staged.state = DisplayTransactionState::AwaitingConfirmation;
        staged.save().unwrap();
        let value = inspect(&chrome);
        assert_eq!(value["waiting"]["transaction"], staged.id);
        assert_eq!(value["waiting"]["keep"], DISPLAY_KEEP_ID);
        assert_eq!(value["layout"], DISPLAY_MIRROR_ID);
        assert_eq!(value["outputs"][1]["mirror_of"], "eDP-1");
        assert_eq!(
            shell_action_kind(DISPLAY_OUTPUTS_INSPECT_ID),
            tilo_actions::OperationKind::View
        );
    }

    #[test]
    fn shared_display_public_actions_inspect_and_edit_only_the_current_certified_pair() {
        let _home = shared_display_actions::test_support::publish();
        let chrome = chrome_with_room();
        let inspect = invoke_shell_action(
            &chrome,
            shared_display_actions::INSPECT_ID,
            &serde_json::json!({}),
        )
        .unwrap();
        let crate::shell::search::InvocationDisposition::Accepted(receipt) = inspect else {
            panic!("inspection completes immediately")
        };
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
            panic!("inspection returns current facts")
        };
        let value = output.value.unwrap();
        let outputs = value["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 3);
        assert!(
            outputs
                .iter()
                .any(|row| row["device"] == "bb02" && row["number"] == 2 && row["revision"] == 0)
        );
        assert!(
            outputs
                .iter()
                .any(|row| row["device"] == "cc03" && row["online"] == false)
        );
        let before = settings_store::load();
        let args =
            serde_json::json!({"device":"bb02","output":"panel","revision":0,"included":false});
        invoke_shell_action(&chrome, shared_display_actions::INCLUDE_ID, &args).unwrap();
        let saved = settings_store::person_displays::Arrangement::load();
        assert!(!saved.entry("bb02", "panel").unwrap().enabled);
        assert!(
            saved.entry("aa01", "panel").unwrap().enabled,
            "same output ID on another device is independent"
        );
        assert_eq!(
            settings_store::load().get("continuity"),
            before.get("continuity"),
            "inclusion cannot grant consent or input"
        );
        let after = settings_store::load();
        assert!(
            invoke_shell_action(&chrome, shared_display_actions::INCLUDE_ID, &args).is_err(),
            "old arrangement revision must fail"
        );
        assert!(invoke_shell_action(&chrome, shared_display_actions::INCLUDE_ID,
            &serde_json::json!({"device":"bb02","output":"connector-guess","revision":0,"included":true})).is_err());
        assert!(invoke_shell_action(&chrome, shared_display_actions::INCLUDE_ID,
            &serde_json::json!({"device":"bb02","output":"panel","revision":0,"included":"true"})).is_err());
        assert_eq!(settings_store::load(), after);
    }

    #[test]
    fn selected_shared_display_action_rechecks_workspace_authority_before_writing() {
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        let _home = shared_display_actions::test_support::publish();
        let chrome = chrome_with_room();
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(Rc::new(ActionsProvider::new()));
        let mut document = AutoCommit::new();
        let resource = ResourceRef::new("actions", shared_display_actions::INCLUDE_ID);
        let catalogue = ActionCatalog::build_exact(
            &registry,
            &resource,
            crate::shell::search::SearchScope::All,
            None,
            &chrome.borrow(),
            &document,
            &automerge::ROOT,
            &AllVisible,
        );
        let action = catalogue
            .select(&resource)
            .expect("published inclusion action");
        chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .unwrap()
            .set_owner("foreign-owner");
        assert!(!current_shell_action_authorized(
            &chrome.borrow(),
            shared_display_actions::INCLUDE_ID
        ));
        let before = settings_store::load();
        assert!(action.invoke_json(&mut ui::widget::Edits::lend(&mut document), &automerge::ROOT,
            &chrome, serde_json::json!({"device":"bb02","output":"panel","revision":0,"included":false}).as_object().unwrap()).is_err());
        assert_eq!(
            settings_store::load(),
            before,
            "revoked authority never saves an arrangement"
        );
    }

    fn chrome_rc() -> crate::shell::chrome_state::Chrome {
        let handle: WorkspaceTreeHandle =
            crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
        ChromeState::new_with_handle(handle)
    }

    /// A session with a room this machine administers — what `agent.invite` needs.
    pub(super) fn chrome_with_room() -> crate::shell::chrome_state::Chrome {
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

    pub(super) fn action_results(
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
        let _home = tilo_test_env::TestHome::new().unwrap();
        let paths = settings_store::SessionPaths::configured().unwrap();
        let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
        let settings = crate::shell::test_support::install_settings(session.host_view().unwrap());
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
            // Held until the dispatch returns: a dispatch that waits on it
            // only ends at the hang bound, and then fails below.
            let responsive = received.recv_timeout(tilo_test_env::hang_bound()).is_ok();
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
        drop(settings);
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
            let settings =
                crate::shell::test_support::install_settings(session.host_view().unwrap());
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
            drop(settings);
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
            let settings =
                crate::shell::test_support::install_settings(session.host_view().unwrap());
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
            drop(settings);
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
        let settings = crate::shell::test_support::install_settings(source.clone());
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
        drop(settings);
    }

    #[test]
    fn published_display_answers_accept_real_durable_record_with_copy_repair_pending() {
        use settings_store::SettingsSource as _;
        use settings_store::hardware::{
            DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState,
        };
        use std::os::unix::fs::PermissionsExt as _;
        for id in [DISPLAY_KEEP_ID, DISPLAY_REVERT_ID] {
            let _home = tilo_test_env::TestHome::new().unwrap();
            let paths = settings_store::SessionPaths::configured().unwrap();
            let session = Arc::new(settings_store::SessionSettings::open(paths.clone()).unwrap());
            let settings =
                crate::shell::test_support::install_settings(session.host_view().unwrap());
            let mut transaction =
                DisplayTransaction::request(DisplaySet::default(), DisplayConfig::default())
                    .unwrap();
            transaction.state = DisplayTransactionState::AwaitingConfirmation;
            transaction.save().unwrap();
            // The copy stays readable (an unreadable copy may hold another
            // process's edit and blocks the write), but its directory takes
            // no new file, so only the copy's replacement fails.
            let copy_dir = paths.copy.parent().unwrap().to_path_buf();
            assert_ne!(Some(copy_dir.as_path()), paths.device.parent());
            let writable = std::fs::metadata(&copy_dir).unwrap().permissions();
            std::fs::set_permissions(&copy_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            let stale_copy = std::fs::read(&paths.copy).unwrap();
            let ticket = begin_display_answer(&chrome_with_room(), id);
            crate::shell::durable_writer::global().flush();
            let accepted = ticket();
            std::fs::set_permissions(&copy_dir, writable).unwrap();
            assert!(accepted.unwrap().is_some());
            assert_eq!(std::fs::read(&paths.copy).unwrap(), stale_copy);
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
            drop(settings);
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
                chrome.borrow().quick_settings.section,
                Some(crate::shell::quick_settings::QsSection::Clipboard)
            );

            invoke_action(&results, "quick-settings.devices", &chrome);
            assert_eq!(
                chrome.borrow().quick_settings.section,
                Some(crate::shell::quick_settings::QsSection::Devices)
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
                    crate::shell::overlay_host::PeerSyncAction::BrowseDirectory,
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
            assert!(
                c.borrow_mut().take_activity_open_request().is_none(),
                "starts closed"
            );
            (action.on_open)(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &c,
                &ActionArgs::none(),
            )
            .expect("a nullary chrome action must run");
            assert!(
                c.borrow_mut().take_activity_open_request().is_some(),
                "invoking the exported action opens the Activity app"
            );
            assert!(
                !c.borrow().overlays.room_activity,
                "Activity is an app's window, not a modal that captures input"
            );
            assert!(
                !c.borrow().overlays.collaborators,
                "the dropdown Activity can also be opened from is dismissed with it"
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
    fn typing_builds_the_catalogue_once() {
        // The catalogue is rebuilt when the settings document changes, and
        // which document that is follows the environment: hold it still, as
        // every test that changes either does, or a neighbour's settings
        // write reads as this test's rebuild.
        let _env = tilo_test_env::lock();
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

        // Nothing typed is the launcher, which actions are not part of: it
        // asks the catalogue nothing.
        assert!(grouped("").is_empty(), "no action is suggested unasked");
        assert_eq!(builds.get(), 0, "the launcher builds no catalogue");
        for needle in ["d", "da", "dar"] {
            assert!(!grouped(needle).is_empty(), "{needle:?} finds actions");
        }
        let found = grouped("dark")
            .into_iter()
            .flat_map(|group| group.results)
            .any(|result| result.id == "appearance.theme.dark");
        assert!(found, "\"dark\" finds the dark theme");
        assert_eq!(
            builds.get(),
            1,
            "built once, typing narrows the retained rows"
        );
    }

    #[test]
    fn actions_with_nothing_to_act_on_are_kept_from_people() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let row = |id: &str, title: &str| {
            SearchResult::nullary(
                ResourceKind::Action,
                PROVIDER_ID,
                id,
                title,
                None,
                0,
                |_, _, _| {},
            )
        };
        let finish = row("dictation.finish", "Finish dictation");
        let close = row("projection.close", "Close the display sheet");
        let keep = row(DISPLAY_KEEP_ID, "Keep display settings");
        {
            let state = chrome.borrow();
            assert!(
                provider.hidden_from_people(&finish, "fi", &state),
                "not dictating"
            );
            assert!(
                provider.hidden_from_people(&close, "close", &state),
                "no sheet open"
            );
            assert!(
                provider.hidden_from_people(&keep, "keep", &state),
                "no change waits"
            );
            assert!(
                !provider.hidden_from_people(&close, "close the display sheet", &state),
                "named exactly, it is offered and says why it can't run"
            );
        }
        {
            let mut state = chrome.borrow_mut();
            state.search_context.dictating = true;
            state.overlays.projection = true;
        }
        let state = chrome.borrow();
        assert!(!provider.hidden_from_people(&finish, "fi", &state));
        assert!(!provider.hidden_from_people(&close, "close", &state));
    }

    /// Action ids name the room a workspace is; an id of the form from
    /// before (`workspace.select.<number>`) still selects the workspace that
    /// number names, and one a workspace carried before it was joined to its
    /// room's own workspace selects that one.
    #[test]
    fn a_workspace_action_named_by_its_old_number_still_selects_it() {
        let _env = tilo_test_env::lock();
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let (workspace, room) = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map(|active| (active.id, active.room()))
            .expect("fixture");
        let old = crate::shell::state::WorkspaceId(0x5eed);
        chrome
            .borrow()
            .tree_mut()
            .workspace_aliases
            .insert(old, room);
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let ctx = || SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let current = provider
            .resolve(&format!("workspace.select.{room}"), ctx())
            .expect("the room names its workspace's action");
        for legacy in [workspace, old] {
            let id = format!("workspace.select.{}", legacy.0);
            let row = provider
                .resolve(&id, ctx())
                .unwrap_or_else(|| panic!("{id} resolves"));
            assert_eq!(row.id, id, "answered under the id it was asked by");
            assert_eq!(row.title, current.title, "the same workspace's action");
        }
        assert!(
            provider
                .resolve("workspace.select.123456789", ctx())
                .is_none()
        );
    }

    #[test]
    fn resolve_does_not_rank() {
        // The catalogue is rebuilt when the settings document changes, and
        // which document that is follows the environment: hold it still, as
        // every test that changes either does, or a neighbour's settings
        // write reads as this test's rebuild.
        let _env = tilo_test_env::lock();
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
        assert_eq!(
            provider.builds.get(),
            0,
            "an exact action builds only its row"
        );
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
    fn exact_table_actions_keep_the_palette_contract_and_presentation() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        for peer_sync in [false, true] {
            chrome.borrow_mut().peer_sync_enabled = peer_sync;
            let state = chrome.borrow();
            let ctx = SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            let offered = provider.query(ctx);
            for action in Action::ALL {
                let listed = row(&offered, action.id);
                let resolved = provider.resolve(action.id, ctx);
                assert_eq!(listed.is_some(), resolved.is_some(), "{}", action.id);
                if let Some((listed, resolved)) = listed.zip(resolved) {
                    assert_eq!(listed.title, resolved.title, "{}", action.id);
                    assert_eq!(listed.subtitle, resolved.subtitle, "{}", action.id);
                    assert_eq!(listed.badge, resolved.badge, "{}", action.id);
                    assert_eq!(listed.shortcut, resolved.shortcut, "{}", action.id);
                    assert_eq!(listed.params, resolved.params, "{}", action.id);
                    assert_eq!(listed.contract, resolved.contract, "{}", action.id);
                    assert_eq!(
                        listed.operation_kind, resolved.operation_kind,
                        "{}",
                        action.id
                    );
                    assert_eq!(
                        listed.on_invoke.is_some(),
                        resolved.on_invoke.is_some(),
                        "{}",
                        action.id
                    );
                    assert_eq!(
                        listed.on_begin_invoke.is_some(),
                        resolved.on_begin_invoke.is_some(),
                        "{}",
                        action.id
                    );
                }
            }
        }
    }

    #[test]
    fn workspace_action_invocations_do_not_rebuild_the_palette_catalogue() {
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        use crate::shell::state::{Workspace, WorkspaceId};

        let provider = Rc::new(ActionsProvider::new());
        let chrome = chrome_with_room();
        let first = chrome.borrow().tree().desktop.active;
        let second = WorkspaceId::new();
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let mut workspace = Workspace::new(second, "Second");
            workspace.set_owner("shell-fixture");
            tree.desktop.workspaces.push(workspace);
        }
        // The smoke journey searches before switching workspaces. Its retained
        // palette rows become stale after each switch and chrome revision bump.
        assert!(!rows_for(&provider, &chrome, "workspace").is_empty());
        assert_eq!(provider.builds.get(), 1);
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(provider.clone());
        let mut host = crate::shell::overlay_host::ShellOverlayHost::with_fixture_content(
            Box::new(ui::widget::Container::new()),
        )
        .with_chrome(chrome.clone());
        let mut doc = AutoCommit::new();
        for _ in 0..40 {
            for (id, expected) in [("workspace.next", second), ("workspace.previous", first)] {
                let resource = ResourceRef::new("actions", id);
                let catalogue = ActionCatalog::build_for_resource(
                    &registry,
                    &resource,
                    crate::shell::search::SearchScope::All,
                    None,
                    &chrome.borrow(),
                    &doc,
                    &automerge::ROOT,
                    &AllVisible,
                );
                catalogue
                    .select(&resource)
                    .expect("workspace action resolves even with stale palette rows")
                    .invoke(
                        &mut ui::widget::Edits::lend(&mut doc),
                        &automerge::ROOT,
                        &chrome,
                        [],
                    )
                    .expect("workspace action succeeds");
                host.run_requested_shortcut_actions();
                assert_eq!(chrome.borrow().tree().desktop.active, expected);
                assert_eq!(
                    provider.builds.get(),
                    1,
                    "invoking one addressed action must not rebuild every palette row"
                );
            }
        }
        drop(host);
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
        assert_eq!(language(&provider).as_deref(), Some("Change language"));
        let _restore = RestoreLocale;
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(crate::shell::i18n::current_locale(), "de-DE");
        assert_eq!(language(&provider).as_deref(), Some("Sprache ändern"));
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(language(&provider).as_deref(), Some("Change language"));

        // Active workspace: governance rows address the workspace now active.
        let (first_room, second, second_room) = {
            use crate::shell::state::{Workspace, WorkspaceId};
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let first_room = tree.desktop.active_workspace().expect("fixture").room();
            let id = WorkspaceId::new();
            let mut workspace = Workspace::new(id, "Second");
            workspace.set_owner("shell-fixture");
            let room = workspace.room();
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
            let located = (workspace.id, workspace.room());
            drop(tree);
            located
        };

        // A tree mutation the chrome made, which it bumps the revision for.
        let switch_to = |provider: &ActionsProvider| {
            row(
                &rows_for(provider, &chrome, ""),
                &format!("workspace.select.{workspace_room}"),
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
            .room();
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
            .room();
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
        assert!(matches!(result, Err(ActionError::Explained(_))));
        assert!(chrome.borrow_mut().take_window_requests().is_empty());
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
            assert_eq!(t(action.label), "Show keyboard shortcuts");
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
                "Show workspace overview",
                "Show all workspaces and their windows",
            ),
            (
                "de-DE",
                "Arbeitsbereichsübersicht zeigen",
                "Alle Arbeitsbereiche und ihre Fenster zeigen",
            ),
        ] {
            ui::i18n::I18nManager::set_locale(locale);
            let editor = Action::ALL
                .iter()
                .find(|action| action.id == "overview.toggle")
                .expect("overview action");
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
        use settings_store::appearance::{HIGH_CONTRAST, MODE};
        // The person's settings records, kept apart from every other test's.
        let _home = tilo_test_env::TestHome::new().unwrap();
        let config = tempfile::tempdir().expect("isolated settings");
        // The session keeps the person's settings, as it does in a session.
        let paths = settings_store::SessionPaths::beside(config.path().join("settings.json"));
        let records = settings_store::SessionSettings::open(paths).expect("records");
        let _settings = crate::shell::test_support::install_settings(std::sync::Arc::new(records));

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
            id_of("language", "appearance.language"),
            "appearance.language"
        );

        // Unset reads as dark, so a change to dark is shown from light: a
        // field edit stores only what differs from what is read now.
        invoke_action(&query("light theme"), "appearance.theme.light", &chrome);
        assert_eq!(MODE.get().as_deref(), Some("light"));
        let generation = chrome.borrow().appearance_generation;
        invoke_action(&query("dark theme"), "appearance.theme.dark", &chrome);
        assert!(
            chrome.borrow().appearance_generation > generation,
            "theme actions must wake the live frame, not only write the store"
        );
        assert_eq!(MODE.get().as_deref(), Some("dark"));
        assert!(!HIGH_CONTRAST.get());

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
    fn a_theme_action_records_the_field_edits_it_makes_with_both_leaves() {
        // The person's settings records, kept apart from every other test's.
        let _home = tilo_test_env::TestHome::new().unwrap();
        let config = tempfile::tempdir().expect("isolated settings");
        let paths = settings_store::SessionPaths::beside(config.path().join("settings.json"));
        let records = settings_store::SessionSettings::open(paths.clone()).expect("records");
        let _settings = crate::shell::test_support::install_settings(std::sync::Arc::new(records));

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
        // High contrast on, so choosing a mode changes both leaves.
        settings_store::appearance::HIGH_CONTRAST
            .set(true)
            .expect("high contrast on");
        let before =
            std::fs::read_to_string(paths.change_log()).map_or(0, |log| log.lines().count());
        invoke_action(&rows, "appearance.theme.light", &chrome);

        let log = std::fs::read_to_string(paths.change_log()).expect("change log");
        let entries: Vec<serde_json::Value> = log
            .lines()
            .skip(before)
            .map(|line| serde_json::from_str(line).expect("one JSON entry per line"))
            .collect();
        let mut leaves: Vec<_> = entries
            .iter()
            .map(|entry| {
                assert_eq!(entry["section"], "appearance");
                // The palette changes settings through Settings' own field
                // edits, so each leaf names the edit that set it, and the
                // palette action that ran it: "light mode, from the palette"
                // stays distinct from the same edit made in Settings.
                assert_eq!(entry["action"], "settings.appearance/field-edit");
                assert_eq!(
                    entry["invoked_by"],
                    serde_json::json!({ "action": "appearance.theme.light" }),
                    "{entry}"
                );
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
            "the action sets both leaves, each attributed to its field edit and the palette action"
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
        assert_eq!(t("search-action-appearance-language"), "Sprache ändern");
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
                assert!(contract.can_be_undone());
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
            // A setting applied on the device itself is undone by undoing
            // the change, never by the workspace's own undo.
            assert_eq!(
                quick_settings_contract().kind,
                tilo_actions::ActionKind::Undoable
            );
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

    #[test]
    fn search_has_a_brightness_row_per_display_and_brighter_moves_the_focused_one() {
        use crate::shell::chrome_state::DisplayBrightness;
        let recorder = Rc::new(RecordingQsActions::default());
        let dispatcher: Rc<dyn crate::shell::quick_settings::actions::QsActions> = recorder.clone();
        let provider = ActionsProvider::with_shared_qs_actions(Rc::new(SharedQsDispatcher::new(
            dispatcher,
            crate::shell::quick_settings::actions::QsActionAvailability::all(),
        )));
        let chrome = chrome_with_room();
        {
            let mut state = chrome.borrow_mut();
            state.quick_settings.displays = vec![
                DisplayBrightness {
                    connector: "eDP-1".into(),
                    name: "Built-in display".into(),
                    level: 0.5,
                },
                DisplayBrightness {
                    connector: "DP-1".into(),
                    name: "Dell U2723".into(),
                    level: 0.3,
                },
            ];
            state.quick_settings.focused_display = Some("DP-1".into());
        }
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
        let row = |id: &str| {
            results
                .iter()
                .find(|result| result.id == id)
                .cloned()
                .unwrap_or_else(|| panic!("no row {id}"))
        };
        let panel = row("quick-settings.brightness.on.eDP-1");
        assert_eq!(panel.title, "Brightness, Built-in display");
        let monitor = row("quick-settings.brightness.on.DP-1");
        assert_eq!(monitor.title, "Brightness, Dell U2723");
        assert_eq!(monitor.badge.as_deref(), Some("30 %"));
        assert!(provider.adjustable(&monitor), "←/→ steps it in place");
        provider
            .adjust(
                &monitor,
                1,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
            )
            .expect("the step lands");
        provider
            .adjust(
                &panel,
                -1,
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
                &chrome,
            )
            .expect("the step lands");
        (row("quick-settings.brightness.up").on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect("brighter runs");
        assert_eq!(
            recorder.calls.borrow().as_slice(),
            [
                "brightness:DP-1:40",
                "brightness:eDP-1:40",
                "brightness:DP-1:50"
            ],
            "each row moves its own display; brighter moves the focused one"
        );
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
                    tilo_test_env::recv("the radio work to start", &fixture.entered),
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
                tilo_test_env::recv("the radio work's wake", &wake_rx);
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
    pub(super) fn invoke_shell_action(
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
                    &ws.room().to_string(),
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

    /// The Agents tab's Remove is an action too: it revokes every grant
    /// naming the agent, membership included, and refuses anyone who does
    /// not administer the room, through the action and the tab alike.
    #[test]
    fn agent_remove_action_revokes_everything_the_agent_holds() {
        let apply = |chrome: &Chrome, id: &str, params: &[ActionParam], name: &str| {
            let action = Action::ALL
                .iter()
                .find(|action| action.id == id)
                .expect("registered");
            let mut values = vec![(
                "name".to_string(),
                crate::shell::search::action_api::ArgValue::Text(name.to_string()),
            )];
            if params.iter().any(|param| param.name == "confirm") {
                values.push((
                    "confirm".to_string(),
                    crate::shell::search::action_api::ArgValue::Boolean(true),
                ));
            }
            let args = ActionArgs::bind(params, values).expect("declared parameters");
            (action.apply)(&mut chrome.borrow_mut(), &args)
        };
        let grants_of = |chrome: &Chrome, agent: &str| {
            let subject = tilo_action_policy::Subject::agent(agent);
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .expect("fixture has a room")
                .policy
                .grants
                .iter()
                .filter(|grant| grant.subject == subject)
                .count()
        };

        // Phase `the_owner_removes_an_admitted_agent`.
        {
            let chrome = chrome_with_room();
            apply(&chrome, "agent.invite", AGENT_INVITE_PARAMS, "Scribe").expect("owner admits");
            assert!(grants_of(&chrome, "scribe") > 0, "admitted");
            apply(&chrome, "agent.remove", AGENT_REMOVE_PARAMS, "  SCRIBE ")
                .expect("the owner may remove the agent, named in any case");
            assert_eq!(
                grants_of(&chrome, "scribe"),
                0,
                "no grant names it any more"
            );
            assert!(
                chrome
                    .borrow()
                    .tree()
                    .desktop
                    .active_workspace()
                    .unwrap()
                    .agent_members_at(tilo_clock::unix_now())
                    .is_empty(),
                "the Agents tab no longer lists it"
            );
            assert!(
                matches!(
                    apply(&chrome, "agent.remove", AGENT_REMOVE_PARAMS, "scribe"),
                    Err(ActionError::Refused(_))
                ),
                "removing an agent that holds nothing says so"
            );
        }

        // Phase `a_non_administrator_is_refused_by_the_action_and_by_the_tab`.
        {
            let chrome = chrome_with_room();
            apply(&chrome, "agent.invite", AGENT_INVITE_PARAMS, "scribe").expect("owner admits");
            chrome
                .borrow_mut()
                .tree_mut()
                .desktop
                .active_workspace_mut()
                .unwrap()
                .set_owner("foreign-owner");
            let before = capabilities(&chrome);
            assert!(matches!(
                apply(&chrome, "agent.remove", AGENT_REMOVE_PARAMS, "scribe"),
                Err(ActionError::Refused(_))
            ));
            assert_eq!(
                crate::shell::membership::remove_agent(&mut chrome.borrow_mut(), "scribe"),
                Err(crate::shell::membership::MembershipError::Unauthorized(
                    crate::shell::membership::AGENT_REMOVE_ACTION
                )),
                "the Agents tab's own entry point refuses a non-administrator too"
            );
            assert_eq!(capabilities(&chrome), before);
        }

        let contract = shell_action_contract("agent.remove");
        assert_eq!(contract.risk, tilo_actions::RiskLevel::Destructive);
        assert_eq!(
            contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
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
    /// The notification centre's controls are actions too: an agent or a
    /// screen reader opens the list, clears it and brings it back as a click
    /// on "Clear all" and its Undo do, by address, with nothing to click.
    #[test]
    fn notifications_open_clear_and_come_back_by_action() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let resolve = |id: &str| {
            let document = AutoCommit::new();
            let state = chrome.borrow();
            provider
                .resolve(
                    id,
                    SearchContext {
                        query_scope: None,
                        include_programmatic: true,
                        needle: "",
                        wanted: &[ResourceKind::Action],
                        chrome: &state,
                        doc: &document,
                        doc_root: &automerge::ROOT,
                    },
                )
                .unwrap_or_else(|| panic!("{id} is published"))
        };
        let run = |result: &crate::shell::search::SearchResult| {
            let mut document = AutoCommit::new();
            (result.on_open)(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none(),
            )
        };

        let open = resolve("notifications.open");
        assert_eq!(open.operation_kind, tilo_actions::OperationKind::View);
        run(&open).expect("opening the list always works");
        {
            let state = chrome.borrow();
            assert!(state.overlays.quick_settings);
            assert_eq!(
                state.quick_settings.nav_stack.last(),
                Some(&crate::shell::quick_settings::QsLevel::Notifications)
            );
        }

        let clear = resolve("notifications.clear");
        assert!(
            matches!(run(&clear), Err(ActionError::Refused(_))),
            "an empty list says there is nothing to clear"
        );
        let id = chrome.borrow_mut().push_notification("Build finished", "");
        chrome.borrow_mut().dismiss_notification(id);
        assert_eq!(chrome.borrow().notification_history().len(), 1);
        run(&clear).expect("a list with something in it clears");
        assert!(chrome.borrow().notification_history().is_empty());
        assert!(chrome.borrow().can_undo_notification_history_clear());

        let undo = resolve("notifications.clear.undo");
        run(&undo).expect("what was just cleared comes back");
        let state = chrome.borrow();
        assert_eq!(state.notification_history().len(), 1);
        assert_eq!(state.notification_history()[0].title, "Build finished");
        assert!(!state.can_undo_notification_history_clear());
        drop(state);
        assert!(
            matches!(run(&undo), Err(ActionError::Refused(_))),
            "nothing is left to bring back"
        );
        for id in ["notifications.clear", "notifications.clear.undo"] {
            assert_eq!(
                shell_action_contract(id).kind,
                tilo_actions::ActionKind::Undoable,
                "{id}"
            );
        }

        // The undo lasts its eight seconds whether or not anything painted.
        let id = chrome.borrow_mut().push_notification("Backup done", "");
        chrome.borrow_mut().dismiss_notification(id);
        run(&clear).expect("a list with something in it clears");
        let ended = ui::time::now() + tilo_desktop::notifications::CLEAR_UNDO_WINDOW;
        ui::time::testing::with_monotonic_time(ended, || {
            let offered = action_available("notifications.clear.undo", &chrome.borrow(), false);
            assert!(!offered && matches!(run(&undo), Err(ActionError::Refused(_))));
        });
    }

    #[test]
    fn radio_details_navigation_actions_are_views_and_do_not_toggle_radios() {
        for (id, section) in [
            (
                "quick-settings.network",
                crate::shell::quick_settings::QsSection::Wifi,
            ),
            (
                "quick-settings.bluetooth",
                crate::shell::quick_settings::QsSection::Bluetooth,
            ),
        ] {
            let provider = ActionsProvider::new();
            let chrome = chrome_with_room();
            let before = (
                chrome.borrow().quick_settings.wifi,
                chrome.borrow().quick_settings.bluetooth,
            );
            let results = action_results(&provider, &chrome);
            let result = results
                .iter()
                .find(|result| result.id == id)
                .expect("published view action");
            assert_eq!(result.operation_kind, tilo_actions::OperationKind::View);
            for _ in 0..2 {
                invoke_action(&results, id, &chrome);
                let state = chrome.borrow();
                assert!(state.overlays.quick_settings);
                // The list opens in place on the root page.
                assert_eq!(
                    state.quick_settings.nav_stack,
                    [crate::shell::quick_settings::QsLevel::Root]
                );
                assert_eq!(state.quick_settings.section, Some(section));
                assert_eq!(
                    (state.quick_settings.wifi, state.quick_settings.bluetooth),
                    before
                );
            }
        }
    }

    #[test]
    fn workspace_rename_names_the_workspace_on_screen_and_refuses_a_bad_name() {
        let chrome = chrome_with_room();
        let name = || {
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.name.clone())
        };
        invoke_shell_action(
            &chrome,
            "workspace.rename",
            &serde_json::json!({ "name": "  Thesis  " }),
        )
        .expect("the person here may rename their workspace");
        assert_eq!(name().as_deref(), Some("Thesis"));
        for bad in ["", "   ", "Line\nbreak"] {
            assert!(
                invoke_shell_action(
                    &chrome,
                    "workspace.rename",
                    &serde_json::json!({ "name": bad }),
                )
                .is_err(),
                "{bad:?} is no name"
            );
        }
        assert!(
            invoke_shell_action(
                &chrome,
                "workspace.rename",
                &serde_json::json!({ "name": "x".repeat(121) }),
            )
            .is_err(),
            "a name is at most 120 characters"
        );
        assert_eq!(name().as_deref(), Some("Thesis"));
    }
}
