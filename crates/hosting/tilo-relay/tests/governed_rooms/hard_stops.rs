//! A relay stopped hard (`podman rm -f`) right after it told a member a
//! write was committed. The answer waits for the relay's own durable copy,
//! not for the room's governor to have written its copy of the record
//! through to the room file, which it does within a second: the next start
//! replays the relay's copy into the governor's, so the write is in the
//! record's history all the same.
use tilo_relay::RelayClient;

use super::*;

/// Copy what `from` holds into `to`, as a hard stop leaves it on disk.
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            // A file removed since it was listed is not part of this state.
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_the_relay_committed_survives_a_hard_stop_before_the_governor_keeps_it() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &host, &laptop, "Lounge").await;
    let mut bob_peer =
        super::sealed_records::join(&relay, &laptop, &room.room, &bob, &phone, [7; 32])
            .await
            .unwrap();
    let mut alice_peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let alice_session = session(&relay, &laptop, room.workspace_id).await;
    let bob_session = session(&relay, &phone, room.workspace_id).await;
    let mut presence = Vec::new();
    receive_until(
        "Alice's replica admits Bob",
        &alice_session,
        &mut alice_peer,
        &mut presence,
        |peer, _| role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor),
    )
    .await;
    crate::common::wait_until_subscribed(&relay, room.workspace_id, &phone.device_pubkey()).await;

    // Alice's first write makes the record, and the governor holds it.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "welcome").unwrap();
    doc.commit_edit("fixture", "topic");
    let message = alice_peer
        .publication_mut()
        .publish_incremental(APP, "", doc.save_after(&[]), &laptop.sk, SystemTime::now())
        .unwrap();
    publish(&alice_session, &message);
    receive_until(
        "the record at Bob",
        &bob_session,
        &mut bob_peer,
        &mut presence,
        |peer, _| text(peer, "topic").as_deref() == Some("welcome"),
    )
    .await;

    // Bob's page commits a write and is told it is committed; the relay is
    // stopped hard at that moment, before the governor's copy is kept.
    let bob_client: RelayClient = bob_session.into_client();
    let write = write(&mut bob_peer, "reply", "committed", &phone.sk);
    super::layout::commit(&bob_client, &phone, room.workspace_id, &write)
        .await
        .expect("the relay commits Bob's write");
    let stopped = tempfile::tempdir().unwrap();
    copy_dir(state.path(), stopped.path());

    // The next start holds Bob's write in the governor's copy of the record.
    let reopened = start(stopped.path(), &alice, &host).await;
    let rooms = reopened.rooms().unwrap();
    tilo_test_env::within("the reopened governor holds Bob's committed write", async {
        loop {
            let held = rooms
                .record_document(&room.room, APP, "")
                .await
                .unwrap()
                .and_then(|bytes| AutoCommit::load(&bytes).ok())
                .and_then(|doc| {
                    let (value, _) = doc.get(ROOT, "reply").ok().flatten()?;
                    value.into_string().ok()
                });
            if held.as_deref() == Some("committed") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
    reopened.shutdown().await;
    alice_session.close().await;
    drop(bob_client);
    relay.shutdown().await;
}
