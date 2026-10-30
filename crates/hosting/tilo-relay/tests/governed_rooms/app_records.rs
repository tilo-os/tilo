//! Opening an app for the first time in a room a home relay governs, after
//! the room lets the app author: a member's device signs the app's empty
//! record itself, as every replica admits a member's record, and the app's
//! first state reaches the record as the member's own change.
use super::*;

const OPENED: &str = "so.tilo.notes";

/// `peer`'s empty record of `app`, signed with `key`, to publish.
fn genesis(
    peer: &mut StatePeer,
    app: &str,
    key: &ed25519_dalek::SigningKey,
) -> Result<NeighbourMsg, String> {
    peer.publication_mut().publish_document(
        app,
        "",
        AutoCommit::new().save(),
        key,
        SystemTime::now(),
    )
}

fn first_state(peer: &StatePeer) -> Option<String> {
    let doc = AutoCommit::load(&peer.document(OPENED, "")?).ok()?;
    doc.get(ROOT, "title")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// Admit `member`'s person to `room` as a Viewer, through an invitation the
/// relay issues and `member`'s device redeems.
async fn admit_viewer(relay: &Relay, host: &TestDevice, member: &TestDevice, room: &str) {
    let issued = relay
        .rooms()
        .unwrap()
        .issue(
            room,
            &host.cert,
            tilo_relay::rooms::IssueInvitationRequest {
                request_id: *blake3::hash(&member.device_pubkey()).as_bytes(),
                role: MemberRole::Viewer,
                recipient_user_public_key: Some(member.cert.user_public_key),
                invitee_name: None,
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(60_000_000_000)
                    .expect("fixture expiry fits"),
            },
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(member.cert.user_public_key),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(device_endpoint(member).await)
        .redeem_hint(
            &verified.grant().transport[0],
            issued.signed_invitation,
            member.cert.clone(),
            &member.sk,
            *blake3::hash(&member.cert.user_public_key).as_bytes(),
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admitted_member_creates_an_authored_apps_empty_record() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let carol = TestUser::fresh("Fixture viewer");
    let host = alice.enrol("private-host", DeviceCapabilities::host());
    let laptop = alice.enrol("private-laptop", DeviceCapabilities::primary());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let viewer = carol.enrol("private-viewer", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let room = create_and_enter(&relay, &laptop, "Private app records").await;
    let workspace = room.workspace_id;
    members::admit(&relay, &host, &member, &room.room).await;
    admit_viewer(&relay, &host, &viewer, &room.room).await;

    let member_session = session(&relay, &member, workspace).await;
    let roster = vec![
        host.cert.clone(),
        laptop.cert.clone(),
        member.cert.clone(),
        viewer.cert.clone(),
    ];
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
    let room_id = room.room.clone();
    let authors = move |peer: &StatePeer| {
        peer.admitted_room().is_some_and(|doc| {
            tilo_desktop_model::projection::room_policy_of(doc, &room_id).app_authors(
                OPENED,
                &tilo_action_policy::Context::at_time(SystemTime::now()),
            )
        })
    };
    receive_until(
        "the member holds the room",
        &member_session,
        &mut member_peer,
        &mut Vec::new(),
        |peer, _| peer.admitted_room().is_some(),
    )
    .await;
    assert!(
        genesis(&mut member_peer.clone(), OPENED, &member.sk).is_err(),
        "a record of an app the room does not let author is never created"
    );

    // The room lets the app author, as its host is configured to.
    assert!(
        relay
            .rooms()
            .unwrap()
            .settle_app_authorship(
                &room.room,
                &host.cert,
                &std::collections::BTreeSet::from([OPENED.to_owned()]),
                &std::collections::BTreeSet::new(),
            )
            .await
            .unwrap()
    );
    let laptop_session = session(&relay, &laptop, workspace).await;
    receive_until(
        "the member holds the app's authorship",
        &member_session,
        &mut member_peer,
        &mut Vec::new(),
        |peer, _| authors(peer),
    )
    .await;
    assert!(
        genesis(&mut member_peer.clone(), "so.tilo.notes\n", &member.sk).is_err(),
        "a malformed application id is refused"
    );
    crate::common::wait_until_subscribed(&relay, workspace, &member.device_pubkey()).await;
    let created = genesis(&mut member_peer, OPENED, &member.sk).unwrap();
    publish(&member_session, &created);
    receive_until(
        "another member's replica holds the member's empty record",
        &laptop_session,
        &mut laptop_peer,
        &mut Vec::new(),
        |peer, _| peer.document(OPENED, "").is_some(),
    )
    .await;
    let mut held = AutoCommit::load(&laptop_peer.document(OPENED, "").unwrap()).unwrap();
    assert!(
        held.get_heads().is_empty(),
        "a member creates an empty record and nothing else"
    );
    assert!(
        genesis(&mut member_peer.clone(), OPENED, &member.sk).is_err(),
        "a record the room holds is never created again"
    );

    // The app's first state is the member's own change onto the genesis.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "First note").unwrap();
    doc.commit_edit("fixture", "title");
    let bootstrap = member_peer
        .publication_mut()
        .publish_incremental(
            OPENED,
            "",
            doc.save_after(&[]),
            &member.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&member_session, &bootstrap);
    receive_until(
        "another member's replica admits the member's first state",
        &laptop_session,
        &mut laptop_peer,
        &mut Vec::new(),
        |peer, _| first_state(peer).as_deref() == Some("First note"),
    )
    .await;

    // A viewer's client that skips its own checks and writes the record
    // anyway, as a modified page could: the relay neither keeps nor
    // forwards the write. Another member's replica would refuse it loudly
    // (`receive_until` unwraps every delta it applies), and only the
    // editor's later write reaches it.
    let viewer_session = session(&relay, &viewer, workspace).await;
    let mut viewer_peer = replica();
    receive_until(
        "the viewer holds the member's first state",
        &viewer_session,
        &mut viewer_peer,
        &mut Vec::new(),
        |peer, _| first_state(peer).as_deref() == Some("First note"),
    )
    .await;
    let retitle = |peer: &mut StatePeer, title: &str, key: &ed25519_dalek::SigningKey| {
        let mut doc = AutoCommit::load(&peer.document(OPENED, "").unwrap()).unwrap();
        let heads = doc.get_heads();
        doc.put(ROOT, "title", title).unwrap();
        doc.commit_edit("fixture", "title");
        peer.publication_mut()
            .publish_incremental(OPENED, "", doc.save_after(&heads), key, SystemTime::now())
            .unwrap()
    };
    // The viewer's own replica refuses to sign the write, so the page
    // builds it from a member's replica and signs it as the viewer.
    let mut forged = retitle(&mut member_peer.clone(), "A viewer's note", &member.sk);
    if let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut forged {
        *peer = tilo_peer_state::peer_handle(&viewer.device_pubkey());
        envelope.author = collab_types::Ed25519PublicKey::from_bytes(viewer.device_pubkey());
        envelope.seq = 1;
        collab_types::sign_op(envelope, &viewer.sk).unwrap();
    }
    publish(&viewer_session, &forged);
    // Past the time the relay takes a write in, so a forwarded forgery
    // would arrive before the editor's write below.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let second = retitle(&mut member_peer, "Second note", &member.sk);
    publish(&member_session, &second);
    receive_until(
        "another member's replica takes the editor's write and nothing of the viewer's",
        &laptop_session,
        &mut laptop_peer,
        &mut Vec::new(),
        |peer, _| first_state(peer).as_deref() == Some("Second note"),
    )
    .await;
    viewer_session.close().await;
    member_session.close().await;
    relay.shutdown().await;
}
