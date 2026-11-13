//! The host actions a session publishes to the surfaces that offer them:
//! what only the session can do about workspaces (making one, joining one,
//! leaving this one, and its settings) and about the session itself (locking
//! the screen, logging out, restarting the computer, shutting it down), and
//! the empty desk's welcome verbs, which the launchpad declares and its cards
//! show. The workspace menu lists the workspace rows, the Tilo menu the
//! session's, and search all of them.
//!
//! Each row is named once: its name, the sentence describing it, its own
//! picture, and whether search may offer it before anything is typed.

use tilo_desktop::desktop_host::{
    BRANCH_WORKSPACE, JOIN_WORKSPACE, LEAVE_WORKSPACE, NEW_WORKSPACE, REPORT_PROBLEM, SESSION_LOCK,
    SESSION_LOG_OUT, SESSION_RESTART, SESSION_SHUT_DOWN, WORKSPACE_SETTINGS,
};
use tilo_desktop::launchpad::{LaunchpadAction, WorkspaceScope};
use tilo_desktop_surface::{DesktopHandle, HostAction, HostActionSource};
use tilo_sheets::power::{Offers, PowerEffect};
use ui::icons::Icon;

/// The session's own rows, in the order a person reads them: what happens to the screen first, what
/// happens to the computer last. Switch user and suspend are the power menu's, not the Tilo menu's.
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

/// The empty desk's cards this device offers: a verb that opens an app stands only where the app is
/// installed here, which is the app catalogue the desk's search lists it from. The desk draws these
/// cards, publishes a row for each to agents, and search lists them; a verb whose app is not here
/// is no card, no row and no search entry.
#[cfg(target_os = "linux")]
pub(crate) fn cards(lists: &crate::lists::Lists) -> Vec<LaunchpadAction> {
    let listed = lists.latest();
    LaunchpadAction::ALL
        .into_iter()
        .filter(|card| card.offered(|app| listed.has_app(app)))
        .collect()
}

/// Whether this device offers `card`: its app is installed here, or the verb happens on the desk
/// itself.
#[cfg(target_os = "linux")]
pub(crate) fn offers(lists: &crate::lists::Lists, card: LaunchpadAction) -> bool {
    card.offered(|app| lists.latest().has_app(app))
}

/// Offer the session's host actions: the workspace rows always, the empty desk's welcome verbs, and
/// the session's own rows where this session can carry them out — a session in a window of another
/// desktop neither restarts nor shuts its host down, and one that cannot be unlocked again is not
/// offered locking.
/// `can_join` is whether a workspace can be joined here at all, which is whether this session holds
/// the directory a workspace is joined from, and `can_report` is whether this computer is told
/// where a report about Tilo itself goes.
pub fn publish(
    handle: &DesktopHandle,
    offers: Offers,
    can_join: bool,
    can_report: bool,
    #[cfg(target_os = "linux")] lists: &crate::lists::Lists,
) {
    let t = tilo_desktop_surface::i18n::t;
    let offer =
        |id: &str, title: &str, description: Option<&str>, icon: Icon, source: HostActionSource| {
            HostAction {
                id: id.to_owned(),
                title: t(title),
                subtitle: description.map(t),
                icon: Some(icon.name().to_owned()),
                destructive: false,
                unasked: false,
                source: Some(source),
            }
        };
    // Making something and bringing someone in are what a person may want to do from the empty
    // card; leaving takes the workspace away from this computer, and its settings are looked for by
    // name.
    let mut actions = vec![HostAction {
        unasked: true,
        ..offer(
            NEW_WORKSPACE,
            "session-host-new-workspace",
            Some("session-host-new-workspace-description"),
            Icon::Plus,
            HostActionSource::Workspace,
        )
    }];
    // Branching is looked for by name: the workspace the person is in, copied into one of their own.
    actions.push(offer(
        BRANCH_WORKSPACE,
        "session-host-branch-workspace",
        Some("session-host-branch-workspace-description"),
        Icon::Copy,
        HostActionSource::Workspace,
    ));
    if can_join {
        actions.push(HostAction {
            unasked: true,
            ..offer(
                JOIN_WORKSPACE,
                "session-host-join-workspace",
                Some("session-host-join-workspace-description"),
                Icon::Link,
                HostActionSource::Workspace,
            )
        });
    }
    actions.push(HostAction {
        destructive: true,
        ..offer(
            LEAVE_WORKSPACE,
            "session-host-leave-workspace",
            Some("session-host-leave-workspace-description"),
            Icon::ArrowUpRight,
            HostActionSource::Workspace,
        )
    });
    actions.push(offer(
        WORKSPACE_SETTINGS,
        "session-host-workspace-settings",
        Some("session-host-workspace-settings-description"),
        Icon::Settings,
        HostActionSource::Workspace,
    ));
    // The empty desk's welcome verbs, declared by the launchpad that shows them: the desk's cards,
    // search and the catalogue agents read all read these rows, and only the verbs whose app this
    // device has are offered.
    #[cfg(target_os = "linux")]
    actions.extend(
        cards(lists)
            .into_iter()
            .map(|card| card.host_action(WorkspaceScope::default())),
    );
    // A report about Tilo itself goes where this computer is told, so a computer told nowhere is
    // offered no report.
    if can_report {
        actions.push(offer(
            REPORT_PROBLEM,
            "session-host-report-problem",
            Some("session-host-report-problem-description"),
            Icon::Flag,
            HostActionSource::Session,
        ));
    }
    for (effect, id, title) in SESSION_ROWS {
        if offers.offers(effect) {
            actions.push(HostAction {
                // Locking stands at the end of what the empty card offers,
                // below making and joining; the rest only leave or end this
                // computer, so they are looked for by name.
                unasked: matches!(effect, PowerEffect::Lock),
                ..offer(id, title, None, effect.icon(), HostActionSource::Session)
            });
        }
    }
    handle.set_host_actions(actions);
}
