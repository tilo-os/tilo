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
use tilo_sheets::directory::{DirectoryEffect, DirectorySeat};

use crate::directory;

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

/// Show the workspaces this computer may join or open: the ones it holds,
/// and what the sites' directories list.
pub(crate) fn open_directory(
    seat: Option<&DirectorySeat>,
    handle: &DesktopHandle,
    directory: &directory::Store,
) {
    let Some(seat) = seat else {
        return;
    };
    seat.clear_query();
    seat.show(directory::view(directory, handle));
    seat.presentation().open(false);
    directory.ask(None);
}

/// Carry out what the directory asked: reading a site's directory again,
/// opening a workspace this computer holds, and closing it.
pub(crate) fn follow_directory(
    seat: Option<&DirectorySeat>,
    handle: &DesktopHandle,
    directory: &directory::Store,
) {
    let Some(seat) = seat else {
        return;
    };
    if directory.answered() {
        seat.show(directory::view(directory, handle));
    }
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
                    directory.set_note(Some(error));
                    seat.show(directory::view(directory, handle));
                    continue;
                }
                seat.presentation().close();
            }
            DirectoryEffect::Look { text } => {
                if let Some(site) = typed_site(&text) {
                    // A site the person typed is asked for what it lists,
                    // and the sheet says it is being read until the worker
                    // answers.
                    directory.ask(Some(site));
                    seat.show(directory::view(directory, handle));
                } else {
                    tracing::debug!(
                        %text,
                        "the workspace directory looks up a site, a link or a code"
                    );
                }
            }
            DirectoryEffect::Refresh => {
                directory.ask(None);
                seat.show(directory::view(directory, handle));
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

/// The site `text` names, when what was typed into the search is one: a
/// word that is no address looks at the list instead.
fn typed_site(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.contains('.') && !text.chars().any(char::is_whitespace))
        .then(|| text.to_owned())
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
