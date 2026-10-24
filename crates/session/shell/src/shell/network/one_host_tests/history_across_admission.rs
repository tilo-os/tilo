//! Admitting someone starts the room's document over from a base: the room
//! holds its state in one change and none of its history. What a person sees
//! of the past is kept elsewhere, and survives it: the note's version
//! history, with who wrote each version, on every device that held it and on
//! the newcomer's, and each device's own record of what happened to its
//! windows.

use std::sync::Arc;

use tilo_user_identity::DeviceCapabilities;

use super::{Device, NOTE_RECORD, NOTES, Person, RoomId, Wakes, hosted_workspace, scratch, until};

impl Device {
    /// Every version of the note in `room` this device holds, each with the
    /// device that wrote it.
    fn note_history(&self, room: RoomId) -> Vec<(automerge::ChangeHash, Vec<u8>)> {
        let versions = |document: &mut automerge::AutoCommit| {
            document
                .get_changes(&[])
                .into_iter()
                .map(|change| (change.hash(), change.actor_id().to_bytes().to_vec()))
                .collect()
        };
        if !self.seals_records(room) {
            let Ok(owner) = self.net().native_owner() else {
                return Vec::new();
            };
            return owner
                .with_current(room, |view| Ok(view.peer().document(NOTES, NOTE_RECORD)))
                .ok()
                .flatten()
                .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                .map_or_else(Vec::new, |mut document| versions(&mut document));
        }
        let Some(keys) = self.note_keys(room) else {
            return Vec::new();
        };
        self.read_notes(room, &keys);
        let mut notes = self.notes.lock().unwrap();
        notes.get_mut(&room).map_or_else(Vec::new, versions)
    }

    /// Whether this device's own record of its desktop still holds `heads`.
    fn desktop_history_holds(&self, heads: &[automerge::ChangeHash]) -> bool {
        let mut tree = self.tree.write();
        heads
            .iter()
            .all(|head| tree.doc.get_change_by_hash(head).is_some())
    }
}

#[test]
#[serial_test::serial]
fn admitting_someone_keeps_the_notes_history_and_who_wrote_it() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    let wakes = Arc::new(Wakes::default());
    let (ada, grace) = (Person::new(41), Person::new(42));
    let p = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("ada"),
        0x0A41,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let q = Device::start(
        &grace,
        "Grace's laptop",
        &dir.path().join("grace"),
        0x0B41,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let (local, room) = hosted_workspace(&p, &[&q], "Garden");

    // Before anyone else is in the room, Ada writes two versions of the
    // note and arranges her windows.
    p.open_window(room, "Notes");
    p.open_notes_first(room);
    until(&[&p], "the note's record on Ada's device", || {
        p.holds_note(room).then_some(())
    });
    p.write_note(room, "Plant the tulips");
    p.write_note(room, "Plant the tulips and the crocuses");
    let ada_key = p.cert().device_public_key.to_vec();
    let before = until(&[&p], "Ada's versions of the note", || {
        let history = p.note_history(room);
        let hers: Vec<_> = history
            .into_iter()
            .filter(|(_, author)| *author == ada_key)
            .map(|(hash, _)| hash)
            .collect();
        (hers.len() >= 2).then_some(hers)
    });
    let desktop_before = p.tree.write().doc.get_heads();

    // Grace is admitted: the room starts over from a base.
    let invitation = p.invite(local, "Grace", Some(grace.user()));
    q.join(&invitation).unwrap();
    let joined = until(&[&p, &q], "Grace to join", || q.joins.lock().unwrap().pop())
        .unwrap_or_else(|error| panic!("Grace joins: {error}"));
    assert_eq!(joined, room);
    until(&[&p, &q], "the note to reach Grace", || {
        (q.note(room).as_deref() == Some("Plant the tulips and the crocuses")).then_some(())
    });

    // Ada still holds every version she wrote, as hers, and what happened
    // to her windows.
    let kept = p.note_history(room);
    for version in &before {
        assert!(
            kept.contains(&(*version, ada_key.clone())),
            "Ada keeps her version {version} as hers: {kept:?}"
        );
    }
    assert!(
        p.desktop_history_holds(&desktop_before),
        "Ada's record of her windows keeps what came before the admission"
    );

    // Grace sees the note's versions from before she was admitted, each as
    // Ada's.
    let theirs = until(
        &[&p, &q],
        "the note's earlier versions on Grace's device",
        || {
            let history = q.note_history(room);
            before
                .iter()
                .all(|version| history.contains(&(*version, ada_key.clone())))
                .then_some(history)
        },
    );
    assert!(
        !theirs.is_empty(),
        "Grace holds the note's history, not only its text"
    );
    drop((p, q));
}
