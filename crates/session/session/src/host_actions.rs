//! What only the session can do, published to the surfaces that offer it:
//! about workspaces (making one, joining one, leaving this one, and its
//! settings) and about the session itself (locking the screen, logging out,
//! restarting the computer, shutting it down). The workspace menu lists the
//! workspace rows, the Tilo menu the session's, and search both.

use tilo_desktop::desktop_host::{
    JOIN_WORKSPACE, LEAVE_WORKSPACE, NEW_WORKSPACE, SESSION_LOCK, SESSION_LOG_OUT, SESSION_RESTART,
    SESSION_SHUT_DOWN, WORKSPACE_SETTINGS,
};
use tilo_desktop_surface::{DesktopHandle, HostAction, HostActionSource};
use tilo_sheets::power::{Offers, PowerEffect};

/// The session's own rows, in the order a person reads them: what happens
/// to the screen first, what happens to the computer last. Switch user and
/// suspend are the power menu's, not the Tilo menu's.
const SESSION_ROWS: [(PowerEffect, &str, &str); 4] = [
    (PowerEffect::Lock, SESSION_LOCK, "session-host-lock"),
    (PowerEffect::LogOut, SESSION_LOG_OUT, "session-host-log-out"),
    (
        PowerEffect::Restart,
        SESSION_RESTART,
        "session-host-restart",
    ),
    (
        PowerEffect::ShutDown,
        SESSION_SHUT_DOWN,
        "session-host-shut-down",
    ),
];

/// Offer what only the session can do: the workspace rows always, and the
/// session's own rows where this session can carry them out — a session in
/// a window of another desktop neither restarts nor shuts its host down,
/// and one that cannot be unlocked again is not offered locking.
///
/// `can_join` is whether a workspace can be joined here at all, which is
/// whether this session holds the directory a workspace is joined from.
pub(crate) fn publish(handle: &DesktopHandle, offers: Offers, can_join: bool) {
    let offer = |id: &str, title: &str, label: Option<&str>, source: HostActionSource| HostAction {
        id: id.to_owned(),
        title: tilo_desktop_surface::i18n::t(title),
        subtitle: None,
        source: Some(source),
        label: label.map(tilo_desktop_surface::i18n::t),
    };
    let workspace = |id: &str, title: &str, label: &str| {
        offer(id, title, Some(label), HostActionSource::Workspace)
    };
    let mut actions = vec![workspace(
        NEW_WORKSPACE,
        "session-host-new-workspace",
        "workspace-menu-new",
    )];
    if can_join {
        actions.push(workspace(
            JOIN_WORKSPACE,
            "session-host-join-workspace",
            "workspace-menu-join",
        ));
    }
    actions.push(workspace(
        LEAVE_WORKSPACE,
        "session-host-leave-workspace",
        "workspace-menu-leave",
    ));
    actions.push(workspace(
        WORKSPACE_SETTINGS,
        "session-host-workspace-settings",
        "workspace-menu-settings",
    ));
    for (effect, id, title) in SESSION_ROWS {
        if offers.offers(effect) {
            actions.push(offer(id, title, None, HostActionSource::Session));
        }
    }
    handle.set_host_actions(actions);
}
