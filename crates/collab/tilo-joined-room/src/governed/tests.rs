use crate::testing::{self, ROOM};
use crate::{ApplicationRecord, DocumentRead, RoomId, open_file};
use automerge::{AutoCommit, ROOT, transaction::Transactable as _};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use std::time::SystemTime;
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::{SeedState, seed_room};

const APP: &str = "so.tilo.chat";

#[test]
fn a_room_this_device_creates_is_governed_here_takes_its_writes_and_reopens() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("owner.json");
    let owner = open_file(&path).unwrap();
    let key = SigningKey::from_bytes(&[92; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[91; 32]), &key, "Desktop");
    let room = RoomId::parse(ROOM).unwrap();
    let now = SystemTime::now();
    let seed = || SeedState {
        room_changes: seed_room(ROOM, &device, std::slice::from_ref(&device)).unwrap(),
        documents: Vec::new(),
    };
    let stranger = SigningKey::from_bytes(&[93; 32]);
    assert!(
        owner
            .create_governed(room, 7, &device, &stranger, seed(), now)
            .is_err(),
        "governed only with the device's own key"
    );
    owner
        .create_governed(room, 7, &device, &key, seed(), now)
        .unwrap();
    assert!(
        owner
            .create_governed(room, 7, &device, &key, seed(), now)
            .is_err(),
        "a room is created once"
    );
    owner
        .with_current(room, |view| {
            assert!(view.governed_here());
            assert_eq!(view.governor(), &device);
            assert_eq!(view.workspace(), 7);
            assert!(view.transport_hint().is_err(), "no host carries it");
            assert!(view.enrollment_invitation().is_err());
            Ok(())
        })
        .unwrap();

    // Its governor creates an app's record and admits its writes, and no
    // host has to take them into custody.
    let route =
        ApplicationRecord::declared(APP, "", "chat.workspace", &["chat.workspace".into()]).unwrap();
    assert!(
        owner
            .prepare_application_record(room, &device, &key, &route, now)
            .unwrap()
    );
    let request = owner
        .with_current(room, |view| {
            let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            let before = chat.get_heads();
            chat.put(ROOT, "topic", "written here").unwrap();
            chat.commit_edit("fixture", "topic");
            let NeighbourMsg::AppDocument { envelope, .. } = view
                .peer()
                .clone()
                .publication_mut()
                .publish_incremental(APP, "", chat.save_after(&before), &key, now)?
            else {
                return Err("not a document write".into());
            };
            Ok(OwnerCommitRequest {
                room: ROOM.into(),
                operation: CommitOperation::Document {
                    app: APP.into(),
                    record: String::new(),
                    envelope,
                },
            })
        })
        .unwrap();
    owner.commit_document(request.clone(), now).unwrap();
    owner
        .commit_document(request, now)
        .expect("an exact retry is answered from what is held");
    assert_eq!(owner.outgoing_status(room, now).unwrap().pending, 0);

    drop(owner);
    let reopened = open_file(&path).unwrap();
    reopened
        .with_current(room, |view| {
            assert!(view.governed_here());
            assert_eq!(view.governor(), &device);
            let DocumentRead::Captured(document) =
                view.read_document(device.device_public_key, APP, "", None, 64 * 1024, now)?
            else {
                return Err("the record reads back".into());
            };
            let chat = AutoCommit::load(&document.bytes).unwrap();
            assert!(
                automerge::ReadDoc::get(&chat, ROOT, "topic")
                    .unwrap()
                    .is_some()
            );
            Ok(())
        })
        .unwrap();
}
