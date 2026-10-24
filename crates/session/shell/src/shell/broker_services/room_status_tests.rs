use super::RoomStatusSource;
use crate::shell::state::{RoomId, WorkspaceTree};
use futures_util::FutureExt as _;
use runtime_collab::RoomStatus;

fn waiting() -> RoomStatus {
    RoomStatus::Waiting {
        reason: crate::shell::i18n::t("workspace-not-ready-no-documents"),
        detail: None,
    }
}

/// An app asking for a room that is still being joined waits, and is
/// answered once the room is ready: by the change itself, not by asking
/// again, and not by any write that leaves the room as it was.
#[test]
fn a_room_being_joined_is_answered_once_it_is_ready() {
    let room = RoomId::new();
    let mut tree = WorkspaceTree::default();
    tree.joining_rooms.insert(room);
    let tree = tree.shared();
    let rooms = RoomStatusSource::new(tree.clone(), None);
    assert_eq!(rooms.status(room), waiting());

    let waker = futures_util::task::noop_waker();
    let mut context = std::task::Context::from_waker(&waker);
    let mut change = Box::pin(rooms.change(room, waiting()));
    assert!(change.poll_unpin(&mut context).is_pending());
    // A write that changes nothing about the room leaves the wait as it is.
    {
        let mut tree = tree.write();
        tree.desktop.room_epoch = tree.desktop.room_epoch.wrapping_add(1);
    }
    assert!(change.poll_unpin(&mut context).is_pending());

    tree.write().joining_rooms.remove(&room);
    assert_eq!(
        change.poll_unpin(&mut context),
        std::task::Poll::Ready(RoomStatus::Ready)
    );
    drop(change);
    drop(rooms);
}

/// A room no host holds, with its workspace only here, is refused with the
/// reason, at once.
#[test]
fn a_room_on_this_device_only_is_refused_with_the_reason() {
    let mut tree = WorkspaceTree::default();
    let (_, room) = tree.create_hosted_workspace("Plans");
    assert!(tree.pending_placements.remove(&room));
    assert_eq!(
        RoomStatusSource::new(tree.shared(), None).status(room),
        RoomStatus::Refused(crate::shell::i18n::t(
            "workspace-on-this-device-only-no-documents"
        ))
    );
}
