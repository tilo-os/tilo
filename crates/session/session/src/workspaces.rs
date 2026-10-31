//! The workspace menu's host rows, the directory they open, and what the
//! session says when a workspace is left: the parts of workspaces the desk
//! itself cannot answer, because they are the session's to know.

use tilo_desktop::desktop_host::{
    JOIN_WORKSPACE, LEAVE_WORKSPACE, NEW_WORKSPACE, WORKSPACE_SETTINGS,
};
use tilo_desktop_surface::navigation::Command;
use tilo_desktop_surface::{
    DesktopHandle, DesktopNotifications, HostAction, HostActionSource,
    notifications::{Notification, NotificationIconKind},
};
use tilo_sheets::Seat as _;
use tilo_sheets::directory::{
    Directory, DirectoryEffect, DirectorySeat, Listing, ListingAction, Status,
};

/// Offer, in the workspace menu and in search, what only the session can do
/// about workspaces: making one, joining one, leaving this one, and its
/// settings.
pub(crate) fn publish_host_actions(handle: &DesktopHandle) {
    let offer = |id: &str, title: &str, label: &str| HostAction {
        id: id.to_owned(),
        title: tilo_desktop_surface::i18n::t(title),
        subtitle: None,
        source: Some(HostActionSource::Workspace),
        label: Some(tilo_desktop_surface::i18n::t(label)),
    };
    handle.set_host_actions(vec![
        offer(
            NEW_WORKSPACE,
            "session-host-new-workspace",
            "workspace-menu-new",
        ),
        offer(
            JOIN_WORKSPACE,
            "session-host-join-workspace",
            "workspace-menu-join",
        ),
        offer(
            LEAVE_WORKSPACE,
            "session-host-leave-workspace",
            "workspace-menu-leave",
        ),
        offer(
            WORKSPACE_SETTINGS,
            "session-host-workspace-settings",
            "workspace-menu-settings",
        ),
    ]);
}

/// The directory's rows: the workspaces this computer holds, which the
/// person opens from it. A site's public ones arrive with the workspace
/// directory.
pub(crate) fn listings(handle: &DesktopHandle) -> Vec<Listing> {
    handle.read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| Listing {
                key: workspace.id.0.to_string(),
                name: workspace.name.clone(),
                action: ListingAction::Open,
                ..Listing::default()
            })
            .collect()
    })
}

/// Show the workspaces this computer may join or open.
pub(crate) fn open_directory(seat: Option<&DirectorySeat>, handle: &DesktopHandle) {
    let Some(seat) = seat else {
        return;
    };
    seat.clear_query();
    seat.show(Directory {
        status: Status::Listed,
        listings: listings(handle),
        note: None,
    });
    seat.presentation().open(false);
}

/// Carry out what the directory asked: opening a workspace this computer
/// holds, and closing it.
pub(crate) fn follow_directory(seat: Option<&DirectorySeat>, handle: &DesktopHandle) {
    let Some(seat) = seat else {
        return;
    };
    for effect in seat.take_asked() {
        match effect {
            DirectoryEffect::Open { key } => {
                let opened = handle.dispatch(
                    &Command::Workspace {
                        generation: crate::GENERATION.into(),
                        workspace: key.clone(),
                    },
                    crate::GENERATION,
                );
                if let Err(error) = opened {
                    // The listing named a workspace the desk no longer
                    // admits: say so where the person picked it, and leave
                    // the directory up to pick another.
                    tracing::warn!(%error, %key, "workspace directory open");
                    let mut directory = seat.directory();
                    directory.note = Some(error);
                    seat.show(directory);
                    continue;
                }
                seat.presentation().close();
            }
            DirectoryEffect::Done => {
                seat.presentation().close();
            }
            other => tracing::debug!(
                ?other,
                "the workspace directory asks for a listing the session has none of"
            ),
        }
    }
}

/// Say the person left the workspace `name`, and what became of its
/// windows.
pub(crate) fn left_workspace(notices: &DesktopNotifications, name: &str) {
    let mut notice = Notification::new(
        tilo_desktop_surface::i18n::t_args("session-left-workspace", &[("name", name.into())]),
        tilo_desktop_surface::i18n::t("session-left-workspace-body"),
    );
    notice.icon_kind = NotificationIconKind::Workspace;
    notices.post(notice);
}
