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
use tilo_sheets::directory::{DirectoryEffect, DirectorySeat, ListingAction};

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
/// joining a public workspace, opening one this computer holds, and closing
/// it.
///
/// `add_workspace` gives this computer the workspace a join was granted, by
/// its room and its name, through the desk's own published action: the desk
/// makes workspaces, not the directory.
pub(crate) fn follow_directory(
    seat: Option<&DirectorySeat>,
    handle: &DesktopHandle,
    directory: &directory::Store,
    add_workspace: &dyn Fn(&directory::Joined) -> Result<(), String>,
) {
    let Some(seat) = seat else {
        return;
    };
    if directory.answered() {
        seat.show(directory::view(directory, handle));
    }
    for joined in directory.take_joined() {
        match add_workspace(&joined) {
            Ok(()) => {
                // The person lands in the workspace they joined, as they do
                // in any other they are given: the directory's work is done.
                seat.presentation().close();
            }
            Err(error) => {
                tracing::warn!(%error, room = %joined.room, "a joined workspace was not added");
                directory.set_note(Some(error));
            }
        }
        seat.show(directory::view(directory, handle));
    }
    for effect in seat.take_asked() {
        match effect {
            DirectoryEffect::Join { key } => {
                join_listed(seat, handle, directory, &key);
            }
            DirectoryEffect::Open { key } => {
                open_listed(seat, handle, directory, &key);
            }
            DirectoryEffect::Look { text } => {
                if let Some(site) = typed_site(&text) {
                    // A site the person typed is asked for what it lists,
                    // and the sheet says it is being read until the worker
                    // answers.
                    directory.ask(Some(site));
                    seat.show(directory::view(directory, handle));
                    continue;
                }
                // One workspace answers the search: Enter takes it, opening
                // it when this computer holds it and joining it otherwise.
                let only: Vec<_> = seat
                    .directory()
                    .listings
                    .into_iter()
                    .filter(|listing| listing.matches(&text))
                    .collect();
                let [only] = only.as_slice() else {
                    tracing::debug!(
                        %text,
                        "the workspace directory looks up a site, a link or a code"
                    );
                    continue;
                };
                if only.action == ListingAction::Open {
                    open_listed(seat, handle, directory, &only.key);
                } else {
                    join_listed(seat, handle, directory, &only.key);
                }
            }
            DirectoryEffect::LinkOrCode { text } => {
                // Joining with a link or an invitation code needs the join
                // field's exchange, which this session does not host.
                tracing::debug!(
                    %text,
                    "the workspace directory hands links and codes to a join field it has none of"
                );
            }
            DirectoryEffect::Refresh => {
                directory.ask(None);
                seat.show(directory::view(directory, handle));
            }
            DirectoryEffect::Done => {
                seat.presentation().close();
            }
        }
    }
}

/// Go to the workspace `key` names, which this computer holds, and be done
/// with the directory. A workspace the desk no longer admits is said where
/// the person picked it, and the directory stays up to pick another.
fn open_listed(
    seat: &DirectorySeat,
    handle: &DesktopHandle,
    directory: &directory::Store,
    key: &str,
) {
    let opened = handle.dispatch(
        &Command::Workspace {
            generation: crate::GENERATION.into(),
            workspace: key.to_owned(),
        },
        crate::GENERATION,
    );
    if let Err(error) = opened {
        tracing::warn!(%error, %key, "workspace directory open");
        directory.set_note(Some(error));
        seat.show(directory::view(directory, handle));
        return;
    }
    seat.presentation().close();
}

/// Join the listing `key` names, one join at a time: the row joining says
/// so, and pressing it again says it is on its way.
fn join_listed(
    seat: &DirectorySeat,
    handle: &DesktopHandle,
    directory: &directory::Store,
    key: &str,
) {
    let Some(listing) = directory.listing(key) else {
        tracing::debug!(%key, "the workspace directory was asked to join a listing it has none of");
        return;
    };
    match directory.joining() {
        Some(joining) if joining == key => {
            directory.set_note(Some(tilo_sheets::words(
                "directory-already-joining",
                &[("name", &listing.entry.name)],
            )));
        }
        // One join at a time: the first is still on its way.
        Some(_) => return,
        None => {
            if let Err(error) = directory.join(key, &listing) {
                directory.set_note(Some(error));
            }
        }
    }
    seat.show(directory::view(directory, handle));
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
