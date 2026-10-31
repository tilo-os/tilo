//! The shell's own actions: what every shell function is reachable as, by a
//! person searching, a chord, a script, an agent or a screen reader.
//!
//! Each action is declared once, in [`Action::ALL`], with its address, its
//! words, its parameters, its contract and operation kind, and what running
//! it does to the chrome. The palette's rows, the authority check and an
//! agent's catalogue all read that declaration; families whose actions are
//! run by a module of their own (membership, pairing, identity
//! administration, enrollment) declare their contract beside their handler,
//! and their entries here name it.
//!
//! Most actions are nullary — "toggle the overview" needs nothing but the
//! instruction. `Action::params` lets one take a typed value instead, for an
//! action whose input isn't discoverable from the chrome and has to be
//! supplied and declared, so enumeration tells a caller what to pass rather
//! than leaving it to be inferred.

use crate::shell::state::WorkspaceAuthority as _;
use settings_store::display_mode::DisplayLayoutMode;
use settings_store::hardware::ShortcutAction;
use std::rc::Rc;

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::i18n::t;
use crate::shell::search::ResourceKind;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};

mod all;
pub(crate) mod device_pairing;
pub(crate) mod enrollment_tokens;
pub(crate) mod host;
pub(crate) mod identity_admin;
pub(crate) mod native_identity;
pub(crate) mod shortcuts;
pub(crate) mod strips;
pub(crate) mod tabs;
pub(crate) mod windows;
pub(crate) mod workspaces;
pub(crate) use shortcuts::action_shortcut;

pub(crate) use strips::valid_strip_name;
use strips::{
    STRIP_CREATE_PARAMS, STRIP_MOVE_PARAMS, STRIP_REMOVE_PARAMS, STRIP_RENAME_PARAMS, create_strip,
    move_strip, remove_strip, rename_strip, show_canvas, show_strips,
};
use tabs::{
    TAB_MOVE_TO_WORKSPACE_PARAMS, TAB_RENAME_PARAMS, move_focused_tab_to_workspace,
    rename_focused_tab,
};
pub(crate) use windows::{
    GROUP_MOVE_TO_STRIP_ID, GROUP_MOVE_TO_STRIP_PARAMS, group_move_to_strip_params,
    move_group_to_strip, no_window, other_display_needed, request_group_shortcut,
    request_toggle_maximize, window_outcome,
};
use workspaces::{
    WORKSPACE_DELETE_PARAMS, delete_workspace, move_workspace, workspace_delete_params,
};

use crate::shell::shared_display_actions;

pub(crate) const WORKSPACE_CREATE_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "name",
    ParamType::Text,
    "1 to 120 characters without control characters. Omit to use the localized default name.",
)];

pub(crate) fn workspace_create_context(
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

pub(crate) fn create_workspace(
    state: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
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
pub(crate) const WORKSPACE_RENAME_PARAMS: &[ActionParam] = &[ActionParam::required(
    "name",
    ParamType::Text,
    "The workspace's new name: 1 to 120 characters without control characters.",
)];

/// Rename the workspace on screen, as its name field in the workspace menu
/// and its settings does.
pub(crate) fn rename_workspace(
    state: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
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
pub(crate) const RECORD_BRING_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "id",
        ParamType::Text,
        "The record's id: `file:<absolute path>`, `tab:<id>`, or a minted id.",
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
pub(crate) const AGENT_INVITE_PARAMS: &[ActionParam] = &[
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
pub(crate) const AGENT_REMOVE_PARAMS: &[ActionParam] = &[
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
pub(crate) const DEFAULT_AGENT_NAME: &str = "Claude";

pub(crate) const IDENTITY_CONNECT_PARAMS: &[ActionParam] = &[
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

pub(crate) const LANGUAGE_PARAMS: &[ActionParam] = &[ActionParam::required(
    "language",
    ParamType::Text,
    "BCP 47 tag or name: en-US, de-DE, English, or German.",
)];

pub(crate) const IDENTITY_SELECT_PARAMS: &[ActionParam] = &[
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
pub(crate) fn live_params(action: &Action, state: &ChromeState) -> Vec<ActionParam> {
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
pub(crate) fn request_identity_setup(st: &mut ChromeState) -> Result<(), ActionError> {
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
pub(crate) fn identity_label(display_name: &str) -> String {
    let name = display_name.trim();
    if name.is_empty() {
        t("search-action-identity-unnamed")
    } else {
        name.to_owned()
    }
}

pub(crate) const SEARCH_PROVIDER_SET_PARAMS: &[ActionParam] = &[
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

pub(crate) const PRESENCE_FOLLOW_PARAMS: &[ActionParam] = &[ActionParam::required(
    "collaborator",
    ParamType::Text,
    "Stable collaborator id returned by the workspace roster.",
)];

pub(crate) const SCREENSHOT_SEND_PARAMS: &[ActionParam] = &[ActionParam::required(
    "target",
    ParamType::Text,
    "`workspace` to save into this workspace's files, or the id of one of your online devices.",
)];

pub(crate) const PRESENCE_STATUS_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "text",
    ParamType::Text,
    "Manual status visible to this workspace; empty clears it.",
)];

pub(crate) const PRESENCE_SHARING_PARAMS: &[ActionParam] = &[ActionParam::required(
    "mode",
    ParamType::Text,
    "off, manual, or automatic.",
)];

pub(crate) const PRESENCE_AVAILABILITY_PARAMS: &[ActionParam] = &[ActionParam::required(
    "availability",
    ParamType::Text,
    "available, focusing, or away.",
)];

pub(crate) const PRESENCE_VISIBILITY_PARAMS: &[ActionParam] = &[ActionParam::required(
    "visibility",
    ParamType::Text,
    "What the people in this workspace see of you: where (your window and pointer), here (only that you are here), or nothing.",
)];

pub(crate) const DEVICE_REVOKE_PARAMS: &[ActionParam] = crate::shell::identity_admin::REVOKE_PARAMS;

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

pub(crate) fn active_workspace_id(
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
pub(crate) fn active_identity(
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

/// Apply one of the room's governance actions, as the person at this
/// device, from the arguments `spec` takes.
pub(crate) fn apply_governance_action(
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

pub(crate) fn active_admin_workspace(
    state: &ChromeState,
) -> Result<
    (
        crate::shell::state::WorkspaceId,
        crate::shell::state::RoomId,
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
    Ok((workspace_id, room))
}

/// The pairing steps answer through a ticket, never through a plain apply.
pub(crate) fn pairing_needs_lifecycle() -> ActionError {
    ActionError::Refused("device pairing requires lifecycle dispatch".into())
}

pub(crate) fn identity_needs_lifecycle() -> ActionError {
    ActionError::Refused("identity administration requires lifecycle dispatch".into())
}

pub(crate) fn require_explicit_confirmation(
    args: &ActionArgs,
    action: &str,
) -> Result<(), ActionError> {
    if args.boolean("confirm") == Some(true) {
        Ok(())
    } else {
        Err(ActionError::Refused(format!(
            "explicit confirmation is required to {action}"
        )))
    }
}

/// The name `peer.invite.named` invites under: the inviter's, carried by
/// the invitation the room's host issues, never the invitee's own claim.
pub(crate) fn name_invited_peer(args: &ActionArgs) -> Result<String, ActionError> {
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

pub(crate) const BOT_ADD_PARAMS: &[ActionParam] = &[
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

pub(crate) const BOT_REMOVE_PARAMS: &[ActionParam] = &[
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

pub(crate) const APP_UNINSTALL_PARAMS: &[ActionParam] = &[
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

pub(crate) const APP_REMOVE_FROM_APPS_PARAMS: &[ActionParam] = &[
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
pub(crate) fn uninstall_app(args: &ActionArgs, remove_from_apps: bool) -> Result<(), ActionError> {
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
pub(crate) fn bot_ask(
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

pub(crate) const WORKSPACE_PUBLISH_PARAMS: &[ActionParam] = &[
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

pub(crate) const WORKSPACE_REPORT_PARAMS: &[ActionParam] = &[ActionParam::required(
    "reason",
    ParamType::Text,
    "What is wrong with this workspace.",
)];

/// The listing `args` publish the active workspace with: their description
/// and role, else the role it is public with now (a fresh one: commenter).
pub(crate) fn publish_listing(
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

pub(crate) const PEER_INVITE_NAMED_PARAMS: &[ActionParam] = &[
    ActionParam::required("peer", ParamType::Text, "The invitee's device identity."),
    ActionParam::required(
        "name",
        ParamType::Text,
        "What to call them in this workspace. Shown beside their cursor and caret.",
    ),
];

pub(crate) mod membership_actions;
// Its ids, parameters and its two decoders travel with it.
pub(crate) use membership_actions::{
    CANCEL_ACTION as INVITATION_CANCEL_ID, INVITATION_ACTION_IDS,
    ISSUE_ACTION as INVITATION_ISSUE_ID, LIST_ACTION as INVITATION_LIST_ID,
    REMOVE_ACTION as MEMBER_REMOVE_ID, RESEND_ACTION as INVITATION_RESEND_ID,
    SET_ROLE_ACTION as MEMBER_ROLE_SET_ID, collaboration_contract, collaboration_operation,
};

pub(crate) fn participant(id: &str) -> Result<crate::shell::state::UserId, ActionError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(ActionError::Refused(
            "participant_id must not be empty".into(),
        ));
    }
    Ok(id.to_string())
}

pub(crate) fn cap_role(
    role: tilo_action_broker::collaboration_admin::CollaborationRole,
) -> crate::shell::state::CapRole {
    use tilo_action_broker::collaboration_admin::CollaborationRole;
    match role {
        CollaborationRole::Viewer => crate::shell::state::CapRole::Viewer,
        CollaborationRole::Commenter => crate::shell::state::CapRole::Commenter,
        CollaborationRole::Editor => crate::shell::state::CapRole::Editor,
    }
}

pub(crate) fn invite_role(
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
pub(crate) fn apply_member_role_set(
    st: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
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
pub(crate) fn apply_member_remove(
    st: &mut ChromeState,
    args: &ActionArgs,
) -> Result<(), ActionError> {
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
pub(crate) fn parse_hex_key(value: &str, what: &str) -> Result<[u8; 32], ActionError> {
    let refused = || ActionError::Refused(format!("{what} must be 64 hex characters"));
    let bytes = hex::decode(value.trim()).map_err(|_| refused())?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| refused())
}

/// The peer-sync request an invitation action asks for, validated before
/// anything is queued.
pub(crate) fn invitation_request(
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

pub(crate) fn invitation_requires_lifecycle_dispatch(
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
pub(crate) fn peer_sync_ticket(
    chrome: Chrome,
    request_id: u64,
) -> crate::shell::search::PendingInvocation {
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
pub(crate) const TAB_OPEN_BESIDE_PARAMS: &[ActionParam] = &[
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

pub(crate) const WINDOW_FOCUS_NTH_PARAMS: &[ActionParam] = &[ActionParam::required(
    "index",
    ParamType::Integer,
    "Which window group of the focused strip, counting from 1 (1 to 9).",
)];

/// Parameters of `strip.focus-nth`.
pub(crate) const STRIP_FOCUS_NTH_PARAMS: &[ActionParam] = &[ActionParam::required(
    "index",
    ParamType::Integer,
    "Which strip of the active workspace, counting from 1 at the top (1 to 9).",
)];

/// Queue `action` for the overlay host, which runs it on the focused group
/// exactly as its chord does.
pub(crate) fn request_window_shortcut(
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
pub(crate) fn set_mode(
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

pub(crate) fn set_language(st: &mut ChromeState, requested: &str) -> Result<(), ActionError> {
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

/// The contract and operation kind declared for the shell action `id`,
/// whichever family declares it.
pub(crate) fn declared(id: &str) -> (tilo_actions::ActionContract, tilo_actions::OperationKind) {
    if crate::shell::notification::is_alarm_action(id) {
        return (
            crate::shell::notification::alarm_action_contract(),
            tilo_actions::OperationKind::Effect,
        );
    }
    if let Some(action) = Action::find(id) {
        return (action.contract(), action.kind);
    }
    if activity_view(id) {
        return (
            tilo_actions::ActionContract::view(),
            tilo_actions::OperationKind::View,
        );
    }
    (
        tilo_actions::ActionContract::default(),
        tilo_actions::OperationKind::Effect,
    )
}

/// The views of a workspace's activity and history offered beside the
/// table: each only opens or acts within its view.
pub(crate) const HISTORY_VIEWS: &[(&str, &str, &str, &str)] = &[
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
];

/// Whether `id` only changes what this person sees: the history views, and
/// the workspace views ([`tilo_desktop::activity::view_actions::workspace_view_descriptors`]):
/// selecting a workspace, exploring or replaying its activity. Selecting a
/// workspace changes nothing in it.
fn activity_view(id: &str) -> bool {
    matches!(id, "room.activity.explore" | "room.activity.replay")
        || id.starts_with("workspace.select.")
        || HISTORY_VIEWS.iter().any(|(view, ..)| *view == id)
}

/// One action the shell offers. `id` is its stable address under
/// `actions/` (`overview.toggle`, `settings.open`, …); `apply` runs it when a
/// person picks it, or a script or agent invokes it, taking the bound
/// arguments and the chrome — a nullary toggle simply ignores what it
/// doesn't use. Its contract and operation kind are declared here, beside
/// it, and every door (the palette, authority, an agent's catalogue) reads
/// them from this declaration.
#[derive(Clone)]
pub(crate) struct Action {
    pub(crate) id: &'static str,
    /// Catalogue key of the row's title, resolved when the row is built.
    pub(crate) label: &'static str,
    /// Catalogue key of the row's subtitle.
    pub(crate) subtitle: &'static str,
    /// Catalogue key of the other words people use for the action ("dark
    /// mode", "night" for Dark appearance), a comma-separated list in every
    /// language: search finds the row by them as by its label.
    pub(crate) keywords: &'static str,
    /// Empty for everything that only needs to be told to happen.
    pub(crate) params: &'static [ActionParam],
    pub(crate) apply: fn(&mut ChromeState, &ActionArgs) -> Result<(), ActionError>,
    /// What it promises: whether it can be undone, its risk, whether it is
    /// confirmed first, whether a retry repeats it.
    pub(crate) contract: Contract,
    /// Whether it changes a document, reaches outside, or only the view.
    pub(crate) kind: tilo_actions::OperationKind,
    /// It changes a setting its row shows, so choosing it leaves the
    /// palette open on the new state: a person flipping Do Not Disturb wants
    /// to see it flip, not lose the palette.
    pub(crate) toggles: bool,
    /// It rearranges the desk through the overlay host, and answers once
    /// that ran with whether anything moved ([`window_outcome`]).
    pub(crate) rearranges: bool,
}

/// The contracts the shell's actions declare.
#[derive(Clone, Copy)]
pub(crate) enum Contract {
    /// [`tilo_actions::ActionContract::default`]: a change undone by its
    /// opposite, run at most once.
    Change,
    /// Only opens, inspects or dismisses a view.
    View,
    /// A view step that advances once more each time it is repeated.
    ViewStep,
    /// A view of something outside the document, such as an invitation
    /// composer.
    OutsideView,
    /// Reaches outside the document.
    Outside,
    /// Reaches outside with the person's authority, asked first.
    Privileged,
    /// Ends something for good, asked first, never retried.
    Destructive,
    /// A display arrangement, undone by the opposite one, safe to repeat.
    DisplayLayout,
    /// As the module running the action declares it for `id`.
    Declared(fn(&str) -> tilo_actions::ActionContract),
}

impl Contract {
    pub(crate) fn of(self, id: &str) -> tilo_actions::ActionContract {
        use tilo_actions::{
            ActionContract, ActionKind, ConfirmationPolicy, RetryPolicy, RiskLevel,
        };
        match self {
            Self::Change => ActionContract::default(),
            Self::View => ActionContract::view(),
            Self::ViewStep => ActionContract {
                retry: RetryPolicy::AtMostOnce,
                ..ActionContract::view()
            },
            Self::OutsideView => ActionContract {
                kind: ActionKind::Outside,
                ..ActionContract::view()
            },
            Self::Outside => ActionContract {
                kind: ActionKind::Outside,
                ..ActionContract::default()
            },
            Self::Privileged => ActionContract {
                kind: ActionKind::Outside,
                risk: RiskLevel::Privileged,
                confirmation: ConfirmationPolicy::Confirm,
                ..ActionContract::default()
            },
            Self::Destructive => ActionContract {
                kind: ActionKind::Outside,
                risk: RiskLevel::Destructive,
                confirmation: ConfirmationPolicy::Confirm,
                retry: RetryPolicy::AtMostOnce,
                ..ActionContract::default()
            },
            Self::DisplayLayout => ActionContract {
                kind: ActionKind::Undoable,
                retry: RetryPolicy::Idempotent,
                ..ActionContract::default()
            },
            Self::Declared(contract) => contract(id),
        }
    }
}

/// A chrome toggle: no parameters, and nothing that can fail.
const fn chrome(
    id: &'static str,
    label: &'static str,
    subtitle: &'static str,
    keywords: &'static str,
    apply: fn(&mut ChromeState, &ActionArgs) -> Result<(), ActionError>,
) -> Action {
    Action {
        id,
        label,
        subtitle,
        keywords,
        params: &[],
        apply,
        contract: Contract::Change,
        kind: tilo_actions::OperationKind::Effect,
        toggles: false,
        rearranges: false,
    }
}

impl Action {
    /// What a declaration starts from before it says otherwise.
    const NEW: Self = chrome("", "", "", "", |_, _| Ok(()));

    const fn with(mut self, contract: Contract, kind: tilo_actions::OperationKind) -> Self {
        self.contract = contract;
        self.kind = kind;
        self
    }
    const fn view(self) -> Self {
        self.with(Contract::View, tilo_actions::OperationKind::View)
    }
    const fn view_step(self) -> Self {
        self.with(Contract::ViewStep, tilo_actions::OperationKind::View)
    }
    const fn outside_view(self) -> Self {
        self.with(Contract::OutsideView, tilo_actions::OperationKind::View)
    }
    const fn outside(mut self) -> Self {
        self.contract = Contract::Outside;
        self
    }
    const fn privileged(mut self) -> Self {
        self.contract = Contract::Privileged;
        self
    }
    const fn destructive(mut self) -> Self {
        self.contract = Contract::Destructive;
        self
    }
    const fn display_layout(mut self) -> Self {
        self.contract = Contract::DisplayLayout;
        self
    }
    const fn declared(mut self, contract: fn(&str) -> tilo_actions::ActionContract) -> Self {
        self.contract = Contract::Declared(contract);
        self
    }
    /// It changes a workspace's document (its name, its strips, a record
    /// brought into it) rather than reaching outside.
    const fn document(mut self) -> Self {
        self.kind = tilo_actions::OperationKind::Document;
        self
    }
    const fn toggles(mut self) -> Self {
        self.toggles = true;
        self
    }
    const fn rearranges(mut self) -> Self {
        self.rearranges = true;
        self
    }

    /// The action declared under `id`.
    pub(crate) fn find(id: &str) -> Option<&'static Self> {
        Self::ALL.iter().find(|action| action.id == id)
    }

    pub(crate) fn contract(&self) -> tilo_actions::ActionContract {
        self.contract.of(self.id)
    }
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
pub(crate) fn display_change_waiting() -> bool {
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
pub(crate) fn action_available(id: &str, chrome: &ChromeState, display_waiting: bool) -> bool {
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
pub(crate) fn decline_bluetooth_pairing() -> Result<(), ActionError> {
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
pub(crate) fn stage_display_layout(
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
pub(crate) fn inspect_display_outputs() -> Result<serde_json::Value, ActionError> {
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
pub(crate) fn settle_display_change(keep: bool) -> Result<(), ActionError> {
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

/// The words a declaration's keywords message lists, in every language the
/// shell has ("dark mode", "dunkelmodus", "night"): what a person types who
/// does not know what the row is called.
pub(crate) fn declared_keywords(key: &str) -> Vec<String> {
    crate::shell::i18n::words_in_every_locale(key)
}

/// The picture of an action, by what it acts on: the most specific family
/// its id names (`appearance.theme.dark` before `appearance`). Families
/// are whole id segments, so `tab` never claims `table.*`.
pub(crate) fn action_icon(id: &str) -> Option<ui::icons::Icon> {
    use ui::icons::Icon;
    const FAMILIES: &[(&str, Icon)] = &[
        ("quick-settings.network", Icon::Wifi),
        ("quick-settings.bluetooth", Icon::Bluetooth),
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
        ("group.toggle-maximize", Icon::Maximize2),
        ("magnifier.zoom-in", Icon::ZoomIn),
        ("magnifier.zoom-out", Icon::ZoomOut),
        ("magnifier", Icon::Search),
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

/// Whether the action `id` changes a setting its row shows
/// ([`Action::toggles`]).
pub(crate) fn toggles_in_place(id: &str) -> bool {
    Action::find(id).is_some_and(|action| action.toggles)
}

/// `ON` or `OFF`, as the search-source switches say it.
pub(crate) fn on_off_badge(on: bool) -> String {
    t(if on {
        "search-action-provider-badge-on"
    } else {
        "search-action-provider-badge-off"
    })
}

/// What a row shows of the setting it changes, so a toggle reads as a
/// switch: on or off, or which mode is current.
pub(crate) fn state_badge(id: &str, state: &ChromeState) -> Option<String> {
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
pub(crate) fn fence_native_selection(
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

/// How the shell runs an action, by what it does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lane {
    /// Runs on the chrome at once.
    Apply,
    /// Runs on the chrome at once and answers a script or agent with what it
    /// read or made (an inspection, a setup code); a person picking it sees
    /// it applied.
    Answer,
    /// Starts something that finishes later (the overlay host rearranging
    /// the desk, a request to the network, a display change), answered once
    /// it has.
    Lifecycle,
}

impl Action {
    pub(crate) fn lane(&self) -> Lane {
        let id = self.id;
        if id == shared_display_actions::INSPECT_ID
            || id == DISPLAY_OUTPUTS_INSPECT_ID
            || enrollment_tokens::role_of(id).is_some()
        {
            Lane::Answer
        } else if self.rearranges
            || matches!(
                id,
                "record.bring"
                    | "peer.invite"
                    | "peer.connect"
                    | "peer.invite.named"
                    | "peer.pair-device"
            )
            || device_pairing::is_pair_action(id)
            || identity_admin::is_identity_action(id)
            || INVITATION_ACTION_IDS.contains(&id)
            || id
                .strip_prefix("display.layout.")
                .and_then(DisplayLayoutMode::from_id)
                .is_some()
            || matches!(id, DISPLAY_KEEP_ID | DISPLAY_REVERT_ID)
        {
            Lane::Lifecycle
        } else {
            Lane::Apply
        }
    }
}

/// Apply `action` to the chrome with `args`, as the person picking it does:
/// refused when the active workspace's authority no longer permits it, and
/// for an action that finishes later, which only [`begin`] starts.
pub(crate) fn apply(
    chrome: &Chrome,
    action: &Action,
    args: &ActionArgs,
) -> Result<(), ActionError> {
    if action.lane() == Lane::Lifecycle && !action.rearranges {
        return Err(ActionError::Refused(format!(
            "{} requires lifecycle dispatch",
            action.id
        )));
    }
    let mut st = chrome.borrow_mut();
    if !current_shell_action_authorized(&st, action.id) {
        return Err(ActionError::Refused(format!(
            "current workspace authority no longer permits {}",
            action.id
        )));
    }
    let outcome = (action.apply)(&mut st, args);
    // Repaint only on success; a refusal changed nothing.
    if outcome.is_ok() {
        st.bump();
    }
    outcome
}

/// Run `action` with `args` for whoever invoked it: the one handler every
/// door reaches. An action that finishes later answers once it has.
#[expect(
    clippy::too_many_lines,
    reason = "each lifecycle family starts its own request and answers with its own ticket"
)]
pub(crate) fn begin(
    chrome: &Chrome,
    action: &'static Action,
    args: &ActionArgs,
) -> Result<crate::shell::search::InvocationDisposition, ActionError> {
    use crate::shell::search::{ActionReceipt, InvocationDisposition};
    match action.lane() {
        Lane::Apply => {
            apply(chrome, action, args)?;
            return Ok(InvocationDisposition::Accepted(ActionReceipt::completed()));
        }
        Lane::Answer => return answer(chrome, action, args).map(InvocationDisposition::Accepted),
        Lane::Lifecycle => {}
    }
    if action.rearranges {
        // A rearrangement the overlay host carries out answers once it ran,
        // with whether anything moved.
        let from = chrome.borrow().window_requests_queued();
        apply(chrome, action, args)?;
        let outcomes = chrome.borrow_mut().answer_window_requests_since(from);
        return Ok(window_outcome(action.id, outcomes));
    }
    if action.id == "record.bring" {
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
                    "current workspace authority no longer permits record.bring".into(),
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
        let ticket: crate::shell::search::PendingInvocation =
            Rc::new(
                move || match chrome.borrow_mut().take_record_request_result(request_id) {
                    None => Ok(None),
                    Some(Ok(())) => Ok(Some(crate::shell::search::ActionReceipt::completed())),
                    Some(Err(error)) => Err(ActionError::Refused(error)),
                },
            );
        Ok(crate::shell::search::InvocationDisposition::Pending(ticket))
    } else if matches!(
        action.id,
        "peer.invite" | "peer.connect" | "peer.invite.named" | "peer.pair-device"
    ) {
        let action_id = action.id;

        let request_id = {
            let mut state = chrome.borrow_mut();
            if !current_shell_action_authorized(&state, action_id) {
                return Err(ActionError::Refused(format!(
                    "current workspace authority no longer permits {action_id}"
                )));
            }
            if !matches!(action_id, "peer.connect" | "peer.pair-device") && !state.may_invite_here()
            {
                return Err(ActionError::Refused(
                    "you do not have permission to invite people to this workspace".into(),
                ));
            }
            let request = match action_id {
                "peer.invite" => crate::shell::overlay_host::PeerSyncAction::PromptInvite,
                "peer.connect" => crate::shell::overlay_host::PeerSyncAction::BrowseDirectory,
                "peer.pair-device" => crate::shell::overlay_host::PeerSyncAction::PairDevice,
                "peer.invite.named" => {
                    let name = name_invited_peer(args)?;
                    crate::shell::overlay_host::PeerSyncAction::IssueInviteFor { name, copy: false }
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
    } else if device_pairing::is_pair_action(action.id) {
        let action_id = action.id;

        device_pairing::begin(
            crate::shell::device_pairing::Pairing::session(),
            chrome,
            action_id,
            args,
        )
    } else if identity_admin::is_identity_action(action.id) {
        let action_id = action.id;

        identity_admin::begin(
            crate::shell::identity_admin::IdentityAdmin::session(),
            chrome,
            action_id,
            args,
        )
    } else if INVITATION_ACTION_IDS.contains(&action.id) {
        let action_id = action.id;

        let request = invitation_request(action_id, args)?;
        let request_id = {
            let mut state = chrome.borrow_mut();
            if !current_shell_action_authorized(&state, action_id) {
                return Err(ActionError::Refused(format!(
                    "current workspace authority no longer permits {action_id}"
                )));
            }
            let workspace = active_workspace_id(&state)?;
            state.begin_peer_sync_request(crate::shell::overlay_host::PeerSyncAction::Network {
                workspace,
                request,
            })
        };
        Ok(crate::shell::search::InvocationDisposition::Pending(
            peer_sync_ticket(chrome.clone(), request_id),
        ))
    } else if let Some(mode) = action
        .id
        .strip_prefix("display.layout.")
        .and_then(DisplayLayoutMode::from_id)
    {
        let action_id = action.id;

        if !current_shell_action_authorized(&chrome.borrow(), action_id) {
            return Err(ActionError::Refused(
                "current workspace authority no longer permits this display layout".into(),
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
    } else if matches!(action.id, DISPLAY_KEEP_ID | DISPLAY_REVERT_ID) {
        use crate::shell::display_persistence::{Answer, answer};
        let action_id = action.id;

        if !current_shell_action_authorized(&chrome.borrow(), action_id) {
            return Err(ActionError::Refused(
                "current workspace authority no longer permits this display answer".into(),
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
    } else {
        Err(ActionError::Refused(format!(
            "{} starts nothing",
            action.id
        )))
    }
}

/// What `action` answers a script or agent with.
fn answer(
    chrome: &Chrome,
    action: &Action,
    args: &ActionArgs,
) -> Result<crate::shell::search::ActionReceipt, ActionError> {
    use crate::shell::search::ActionReceipt;
    let id = action.id;
    if id == shared_display_actions::INSPECT_ID {
        return shared_display_actions::inspect().map(ActionReceipt::completed_with);
    }
    if id == DISPLAY_OUTPUTS_INSPECT_ID {
        return inspect_display_outputs().map(ActionReceipt::completed_with);
    }
    // The setup code is the action's answer, for a script or agent to hand
    // to the joining device.
    let role = enrollment_tokens::role_of(id)
        .ok_or_else(|| ActionError::Refused(format!("{id} mints no setup code")))?;
    let state = chrome.borrow();
    if !current_shell_action_authorized(&state, id) {
        return Err(ActionError::Refused(format!(
            "current workspace authority no longer permits {id}"
        )));
    }
    enrollment_tokens::mint(&state, args, role).map(ActionReceipt::completed_with)
}
