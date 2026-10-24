//! One app opening another through the shell's tools service.
//!
//! An app may open another app, or a system tool (Disks, Backups, VPN,
//! Firewall, Terminal), in two cases only. Either the person's own press
//! in one of its windows is behind the request
//! ([`crate::compositor::activation::person_initiated`]: a click, a key, a
//! tap the compositor delivered to it, never input an agent or a remote
//! participant sends through the shared seat), or the app makes it for an
//! invocation the action broker delivered to it, which the broker admits as
//! that invocation's caller and journals as its child
//! ([`tilo_action_broker::collab_broker::AppActionBroker::admit_app_open`]).
//! No app opens another in the background.
//!
//! Either way the app is told only that its request was taken
//! ([`REQUESTED`]). What follows is the person's to see: an installed app
//! opens; one the catalogue offers but this computer doesn't have stands in
//! front of them as the shell's Get card; any other id becomes a notice; a
//! launch that fails says so in a notification. So no app learns from the
//! answer what is installed.

use os_actions::{ToolIntent, ToolRequest};
use tilo_action_broker::collab_broker_capnp::AppOpen;

use crate::shell::chrome_state::Chrome;
use crate::shell::state::{Tab, TileContent, WorkspaceTree};

/// The one answer an app gets for an open the shell took.
pub(crate) const REQUESTED: &str = "requested";

/// The one answer an app gets for an open nothing is behind.
pub(crate) const REFUSED: &str =
    "an app opens another only for the person's press in it or an invocation delivered to it";

/// Whether `intent` opens an app or a system tool, and so needs the person's
/// press or a delivered invocation behind it. The shell's own sheets
/// (workspace settings, identity backup, a drive, an uninstall) ask the
/// person themselves, and lists open nothing.
pub(crate) fn opens_app(intent: &ToolIntent) -> bool {
    intent.package_id().is_some() || matches!(intent, ToolIntent::OpenApplication { .. })
}

/// The app `intent` opens, as the broker's policy names it: a Tilo package
/// by its id, any other listed app as a host application. An id nothing
/// lists is named as the package the catalogue would offer.
pub(crate) fn target(intent: &ToolIntent) -> Option<String> {
    match intent {
        ToolIntent::OpenApplication { app } => Some(match crate::shell::app_list::find(app) {
            Some(item)
                if !matches!(item.source, crate::shell::app_list::AppSource::Tilo { .. }) =>
            {
                format!(
                    "{}{}",
                    crate::shell::broker_services::HOST_APPLICATION_PREFIX,
                    item.id
                )
            }
            _ => app.clone(),
        }),
        other => other.package_id().map(str::to_owned),
    }
}

/// The app that asks, as the shell verified it from the process on the
/// other end of the tools service: never from the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Caller {
    pub package: String,
    pub workspace: String,
}

/// What made an app's open its to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Authority {
    /// The person's press in one of its windows, now used up.
    Press,
    /// An invocation the broker delivered to it, admitted for this open.
    Invocation,
}

/// Whether `request` from `caller` may open `target` ([`target`]): the broker
/// admits it for the invocation it names, or the person's press in one of
/// the caller's windows (taken from `pressed`, which uses it up) is behind
/// it. A request naming someone else's invocation is decided by the broker
/// alone: an agent's open never borrows the person's press. One naming an
/// invocation the app made of itself needs that press.
pub(crate) fn authorize(
    caller: Option<&Caller>,
    request: &ToolRequest,
    target: &str,
    pressed: impl FnOnce(&str) -> bool,
    admit: impl FnOnce(&Caller, tilo_actions::InvocationId, &str) -> Result<AppOpen, String>,
) -> Option<Authority> {
    let caller = caller?;
    if caller.workspace != request.workspace_id {
        return None;
    }
    match &request.on_behalf_of {
        Some(hex) => {
            let invocation = parse_invocation(hex)?;
            match admit(caller, invocation, target) {
                Ok(AppOpen::Admitted) => Some(Authority::Invocation),
                Ok(AppOpen::NeedsPress) => pressed(&caller.package).then_some(Authority::Press),
                Err(reason) => {
                    tracing::info!(app = %caller.package, %target, %reason, "app's open for an invocation refused");
                    None
                }
            }
        }
        None => pressed(&caller.package).then_some(Authority::Press),
    }
}

/// An invocation id as [`os_actions::invocation_hex`] writes it.
fn parse_invocation(hex: &str) -> Option<tilo_actions::InvocationId> {
    (hex.len() == 32
        && hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| uuid::Uuid::parse_str(hex).ok())
    .flatten()
    .filter(|id| !id.is_nil())
    .map(tilo_actions::InvocationId)
}

/// What an authorized open becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// Launch it: an installed app, or a system tool.
    Launch,
    /// The catalogue offers it under this name, but it isn't installed.
    Offer(String),
    /// Nothing is called that, here or in the catalogue.
    Unavailable,
}

/// What `intent` becomes, given what `installed` says is on this computer
/// and what `catalogue` offers (read only for an app that isn't installed).
pub(crate) fn resolve(
    intent: &ToolIntent,
    installed: impl FnOnce(&str) -> bool,
    catalogue: impl FnOnce() -> Vec<crate::shell::catalogue::CatalogueApp>,
) -> Resolution {
    let ToolIntent::OpenApplication { app } = intent else {
        return Resolution::Launch;
    };
    if installed(app) {
        return Resolution::Launch;
    }
    catalogue()
        .iter()
        .find(|offered| crate::shell::launcher::app_id_matches_package(app, &offered.package_id))
        .map_or(Resolution::Unavailable, |offered| {
            Resolution::Offer(offered.display_name.clone())
        })
}

/// Whether app `id` is on this computer: a listed app of any kind, or an
/// installed Tilo package the list has not read yet.
pub(crate) fn installed(id: &str) -> bool {
    crate::shell::app_list::find(id).is_some()
        || crate::shell::launcher::installed_apps()
            .is_ok_and(|apps| apps.iter().any(|app| app.package_id == id))
}

/// Put `resolution` in front of the person, when it is not a launch: the
/// Get card for an app the catalogue offers, beside the asking app's window
/// in its workspace, or a notice that there is no such app.
pub(crate) fn show(chrome: &Chrome, caller: &Caller, app: &str, resolution: &Resolution) {
    match resolution {
        Resolution::Launch => {}
        Resolution::Offer(display_name) => {
            let placed = {
                let state = chrome.borrow();
                let mut tree = state.tree_mut();
                offer(&mut tree, caller, app, display_name)
            };
            if placed {
                chrome.borrow_mut().bump();
            } else {
                notify_unavailable(chrome, caller);
            }
        }
        Resolution::Unavailable => notify_unavailable(chrome, caller),
    }
}

/// Place the Get card for `app` (called `display_name`) in the caller's
/// workspace, right of the caller's window when it shows one there.
/// `false` when that workspace is not on this computer.
fn offer(tree: &mut WorkspaceTree, caller: &Caller, app: &str, display_name: &str) -> bool {
    use crate::shell::tab_binding::Placement;
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room().to_string() == caller.workspace)
    else {
        return false;
    };
    let workspace_id = workspace.id;
    let placement = crate::shell::placement::group_showing(workspace, &caller.package)
        .map_or(Placement::NewGroup, Placement::RightOf);
    let tab = Tab {
        id: crate::shell::state::next_id(),
        title: display_name.to_owned(),
        app_title: None,
        tile: TileContent::LocalApp {
            app_id: app.to_owned(),
            label: display_name.to_owned(),
            note: crate::shell::peer_windows::NOT_INSTALLED.to_owned(),
        },
    };
    let previous = tree.desktop.active;
    tree.desktop.active = workspace_id;
    let placed = crate::shell::tab_binding::place_tab(tree, tab, placement);
    tree.desktop.active = previous;
    // The card is the room's like any window, and closable like one: it
    // enters the room document as well as the cache.
    let strip = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .find(|strip| {
            strip
                .window_groups
                .iter()
                .any(|group| group.tabs.iter().any(|tab| tab.id == placed))
        })
        .map(|strip| (strip.id, strip.window_groups.clone()));
    let Some((strip, groups)) = strip else {
        return false;
    };
    tilo_desktop_model::desktop_doc::rewrite_strip_groups(&mut tree.doc, strip, &groups);
    tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
    tree.queue_save();
    true
}

/// The name the person knows app `package` by.
fn app_name(package: &str) -> String {
    crate::shell::launcher::installed_apps()
        .ok()
        .and_then(|apps| {
            apps.into_iter()
                .find(|app| app.package_id == package)
                .map(|app| app.display_name)
        })
        .unwrap_or_else(|| package.to_owned())
}

/// Tell the person the caller asked to open an app there is none of. Only
/// the caller's name is shown: nothing the request carried.
fn notify_unavailable(chrome: &Chrome, caller: &Caller) {
    use tilo_desktop::notifications::{Notification, NotificationSeverity};
    let mut state = chrome.borrow_mut();
    state.push_notification_full(
        Notification::new(
            crate::shell::i18n::t("app-open-unavailable-title"),
            crate::shell::i18n::t_args(
                "app-open-unavailable-body",
                &[("app", app_name(&caller.package).into())],
            ),
        )
        .with_severity(NotificationSeverity::Info)
        .with_app_name(crate::shell::i18n::t("host-source-applications"))
        .with_dismiss_after(crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
    );
    state.bump();
}

/// A launch the tools service started for an app that failed: what the
/// person is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FailedOpen {
    pub app: String,
    pub reason: String,
}

/// Tell the person each open in `failed` did not happen, and why.
pub(crate) fn notify_failed(chrome: &Chrome, failed: Vec<FailedOpen>) {
    use tilo_desktop::notifications::{Notification, NotificationSeverity};
    if failed.is_empty() {
        return;
    }
    let mut state = chrome.borrow_mut();
    for open in failed {
        state.push_notification_full(
            Notification::new(
                crate::shell::launcher::launch_failure_title(&app_name(&open.app)),
                open.reason,
            )
            .with_severity(NotificationSeverity::Error)
            .with_app_name(crate::shell::i18n::t("host-source-applications"))
            .with_dismiss_after(crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
        );
    }
    state.bump();
}

/// Carry out an authorized open `resolve` made a launch of, in `room`.
///
/// # Errors
///
/// What the person is told when it could not be opened.
pub(crate) fn launch(intent: &ToolIntent, room: crate::shell::state::RoomId) -> Result<(), String> {
    if let Some(argv) = intent.terminal_argv() {
        return crate::shell::launcher::launch_terminal_command(&argv, room)
            .map(drop)
            .map_err(|error| error.user_message());
    }
    match intent {
        ToolIntent::OpenApplication { app } if crate::shell::app_list::find(app).is_some() => {
            crate::shell::app_list::open(app, room)
        }
        ToolIntent::OpenApplication { app } => crate::shell::launcher::launch(app, room)
            .map(drop)
            .map_err(|error| error.user_message()),
        other => crate::shell::launcher::launch_tool_intent(other, room)
            .map(drop)
            .map_err(|error| error.user_message()),
    }
}

/// The app `intent` opens, for the notification of a failed launch.
pub(crate) fn opened_app(intent: &ToolIntent) -> String {
    match intent {
        ToolIntent::OpenApplication { app } => app.clone(),
        other => other.package_id().unwrap_or_default().to_owned(),
    }
}

#[cfg(test)]
#[path = "app_opens_tests.rs"]
mod tests;
