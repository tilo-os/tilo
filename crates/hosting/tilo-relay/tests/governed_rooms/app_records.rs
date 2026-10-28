//! Opening an app for the first time in a room a home relay governs, after
//! the room lets the app author: the app's first state is the member's own
//! signed record, which the relay and every member's replica admit. No host
//! signs a record on a member's behalf.
use super::governance_actions::subscribed;
use super::*;

const OPENED: &str = "so.tilo.notes";

fn first_state(peer: &StatePeer) -> Option<String> {
    let doc = AutoCommit::load(&peer.document(OPENED, "")?).ok()?;
    doc.get(ROOT, "title")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_authors_an_apps_first_record_themselves() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let host = alice.enrol("private-host", crate::common::governor_capabilities());
    let laptop = alice.enrol("private-laptop", DeviceCapabilities::primary());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &host, &laptop, "Private app records").await;
    let workspace = room.workspace_id;
    super::members::admit(&relay, &host, &member, &room.room).await;
    let generation = relay
        .rooms()
        .unwrap()
        .summary(&room.room)
        .await
        .unwrap()
        .generation;

    let member_session = session(&relay, &member, workspace).await;
    let laptop_session = session(&relay, &laptop, workspace).await;
    let roster = vec![host.cert.clone(), laptop.cert.clone(), member.cert.clone()];
    let replica = || {
        let mut peer = StatePeer::new(
            &room.room,
            host.cert.clone(),
            roster.clone(),
            RevocationList::new(),
            SystemTime::now(),
        )
        .unwrap();
        peer.select_admission_v5().unwrap();
        peer
    };
    let mut member_peer = replica();
    let mut laptop_peer = replica();
    // The app's record is the member's own signed change, made where the
    // record is: no empty record of the host's ever precedes it.
    crate::common::wait_until_subscribed(&relay, workspace, &member.device_pubkey()).await;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "First note").unwrap();
    doc.commit_edit("fixture", "title");
    let first = member_peer
        .publication_mut()
        .publish_incremental(
            OPENED,
            "",
            doc.save_after(&[]),
            &member.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&member_session, &first);
    receive_until(
        "the member's own replica holds the record it made",
        &laptop_session,
        &mut laptop_peer,
        &mut Vec::new(),
        |peer, _| first_state(peer).as_deref() == Some("First note"),
    )
    .await;
    assert!(
        relay
            .rooms()
            .unwrap()
            .summary(&room.room)
            .await
            .unwrap()
            .generation
            > generation,
        "the relay keeps the member's record as a change of its own"
    );

    // The member's later writes land on the record they made.
    let mut later = AutoCommit::load(&member_peer.document(OPENED, "").unwrap()).unwrap();
    let heads = later.get_heads();
    later.put(ROOT, "title", "Second note").unwrap();
    later.commit_edit("fixture", "title");
    let second = member_peer
        .publication_mut()
        .publish_incremental(
            OPENED,
            "",
            later.save_after(&heads),
            &member.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&member_session, &second);
    receive_until(
        "another replica takes the member's next write",
        &laptop_session,
        &mut laptop_peer,
        &mut Vec::new(),
        |peer, _| first_state(peer).as_deref() == Some("Second note"),
    )
    .await;

    // The room's owner replica holds the record the member authored.
    let owner_session = session(&relay, &laptop, workspace).await;
    let mut owner_peer = replica();
    receive_until(
        "the room's owner replica holds the record the member authored",
        &owner_session,
        &mut owner_peer,
        &mut Vec::new(),
        |peer, _| first_state(peer).as_deref() == Some("Second note"),
    )
    .await;

    member_session.close().await;
    owner_session.close().await;
    // The member who made its own record is still offered the room's member
    // actions afterwards.
    let editor = subscribed(&relay, &member, workspace).await;
    assert!(!editor.is_closed(), "the member's session stays usable");
    relay.shutdown().await;
}
