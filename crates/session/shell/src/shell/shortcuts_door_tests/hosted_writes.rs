//! A shortcut saved through the door in a room its device hosts is the
//! room's: it reaches the room's other members as its saver saved it.

use tilo_actions::remote::ReceiptStatus;

use super::{Door, owner_principal, publish_draft, room_workspace};
use crate::shell::network::one_host_tests::until;

#[test]
fn a_shortcut_saved_through_the_door_reaches_a_member_as_its_savers() {
    let door = Door::open();
    let bob = door.hosted.bob_joins(&door.dir.path().join("bob"));

    let shortcut = publish_draft("publish-draft", &["draft", "publish"]);
    match door.save(&shortcut).expect("the shortcut is saved") {
        ReceiptStatus::Completed { .. } => {}
        other => panic!("saving completes at once: {other:?}"),
    }
    let owner = owner_principal();
    until(
        &[&door.hosted.device, &bob],
        "the saved shortcut to reach Bob's device",
        || {
            room_workspace(&bob.tree.read(), door.room)
                .shortcuts
                .iter()
                .any(|saved| {
                    saved.id == "publish-draft" && saved.saved_by.as_deref() == Some(owner.as_str())
                })
                .then_some(())
        },
    );
    drop(bob);
    drop(door);
}
