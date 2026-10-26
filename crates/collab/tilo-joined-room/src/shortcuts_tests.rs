use super::*;
use crate::application_record::tests::{Fixture, fixture};
use tilo_automation::{Shortcut, ShortcutChange};

fn shortcut(id: &str) -> Shortcut {
    Shortcut {
        id: id.into(),
        name: id.into(),
        inputs: Vec::new(),
        steps: Vec::new(),
        step_timeout_secs: None,
        output: None,
        grant: Vec::new(),
        triggers: Vec::new(),
        saved_by: None,
        saved_proof: None,
    }
}

fn request(fixture: &Fixture, change: &ShortcutChange) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: crate::testing::ROOM.into(),
        operation: CommitOperation::ShortcutChange {
            invocation: uuid::Uuid::new_v4(),
            caller: fixture.certificate.clone(),
            change: serde_json::to_vec(change).unwrap(),
        },
    }
}

fn saved(owner: &NativeWorkspaceOwner) -> Vec<Shortcut> {
    owner
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            Ok(tilo_desktop_model::projection::workspace_shortcuts_of(
                view.peer().admitted_room().ok_or("no room")?,
            ))
        })
        .unwrap()
}

#[test]
fn a_saved_shortcut_is_a_governed_room_write_that_survives_a_reopen() {
    let fixture = fixture();
    let mut water = shortcut("water");
    tilo_peer_state::room::sign_saved_shortcut(
        crate::testing::ROOM,
        &mut water,
        &fixture.certificate,
        &fixture.key,
    )
    .unwrap();
    let save = request(&fixture, &ShortcutChange::Save(Box::new(water.clone())));
    let receipt = fixture
        .owner
        .commit_shortcut_change(&save, &fixture.certificate, &fixture.key, fixture.now)
        .unwrap();
    receipt.validate_for(&save).unwrap();
    assert_eq!(saved(&fixture.owner), vec![water.clone()]);
    let reopened = crate::open_file(&fixture.directory.path().join("owner.json")).unwrap();
    assert_eq!(saved(&reopened), vec![water]);
    let again = reopened
        .commit_shortcut_change(&save, &fixture.certificate, &fixture.key, fixture.now)
        .unwrap();
    assert_eq!(
        again.generation, receipt.generation,
        "nothing new to commit"
    );

    let delete = request(&fixture, &ShortcutChange::Delete("water".into()));
    reopened
        .commit_shortcut_change(&delete, &fixture.certificate, &fixture.key, fixture.now)
        .unwrap();
    assert!(saved(&reopened).is_empty());
}

#[test]
fn the_governor_cannot_save_a_shortcut_as_someone_else() {
    let fixture = fixture();
    let mut forged = shortcut("water");
    forged.saved_by = Some("42".repeat(32));
    let save = request(&fixture, &ShortcutChange::Save(Box::new(forged)));
    assert!(
        fixture
            .owner
            .commit_shortcut_change(&save, &fixture.certificate, &fixture.key, fixture.now)
            .is_err()
    );
    assert!(saved(&fixture.owner).is_empty());
}
