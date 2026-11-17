//! Opening an app for the first time in a workspace a home relay governs, after
//! the workspace lets the app author: a member's device signs the app's empty
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

/// Admit `member`'s person to `workspace` as a Viewer, through an invitation the
/// relay issues and `member`'s device redeems.
async fn admit_viewer(relay: &Relay, host: &TestDevice, member: &TestDevice, workspace: &str) {
    let issued = relay
        .workspaces()
        .unwrap()
        .issue(
            workspace,
            &host.cert,
            tilo_relay::workspaces::IssueInvitationRequest {
                request_id: *blake3::hash(&member.device_pubkey()).as_bytes(),
                role: Role::Viewer,
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
async fn an_admitted_member_creates_an_apps_empty_record() {
    let alice = TestUser::fresh("Fixture governor");
    let bob = TestUser::fresh("Fixture editor");
    let carol = TestUser::fresh("Fixture viewer");
    let host = alice.enrol("private-host", DeviceCapabilities::member());
    let laptop = alice.enrol("private-laptop", DeviceCapabilities::primary());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let viewer = carol.enrol("private-viewer", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let hosted = create_and_enter(&relay, &laptop, "Private app records").await;
    let workspace = hosted.workspace_id;
    members::admit(&relay, &host, &member, &hosted.workspace).await;
    admit_viewer(&relay, &host, &viewer, &hosted.workspace).await;

    let member_session = session(&relay, &member, workspace).await;
    let roster = vec![
        host.cert.clone(),
        laptop.cert.clone(),
        member.cert.clone(),
        viewer.cert.clone(),
    ];
    let replica = || {
        let mut peer = StatePeer::new(
            &hosted.workspace,
            host.cert.clone(),
            roster.clone(),
            RevocationList::new(),
            SystemTime::now(),
        )
        .unwrap();
        peer.select_exact_admissions().unwrap();
        peer
    };
    let mut member_peer = replica();
    let mut laptop_peer = replica();
    receive_until(
        "the member holds the workspace",
        &member_session,
        &mut member_peer,
        &mut Vec::new(),
        |peer, _| peer.admitted_workspace().is_some(),
    )
    .await;
    // An editor's own change of an app's record is theirs: the record's
    // genesis is theirs to start, and it is published only under an
    // application id.
    let laptop_session = session(&relay, &laptop, workspace).await;
    assert!(
        genesis(&mut member_peer.clone(), "so.tilo.notes\n", &member.sk).is_err(),
        "a malformed application id is refused"
    );
    for malformed in ["", " ", "notes", "so..notes", ".so.tilo", "so.tilo.notes/x"] {
        assert!(
            genesis(&mut member_peer.clone(), malformed, &member.sk).is_err(),
            "{malformed:?} is not an application id"
        );
    }
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
        "a record the workspace holds is never created again"
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

/// What a record of the workspace names, in any version of it, stays in the
/// workspace's content on its host; what nothing names any more goes, once
/// its writer had time to name it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_lets_go_of_content_no_record_of_the_workspace_names() {
    let alice = TestUser::fresh("Fixture governor");
    let host = alice.enrol("collecting-host", DeviceCapabilities::member());
    let laptop = alice.enrol("collecting-laptop", DeviceCapabilities::primary());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let hosted = create_and_enter(&relay, &laptop, "Collected content").await;
    let workspaces = relay.workspaces().unwrap().clone();
    let stores = relay.content();
    let store = stores.get(&hosted.workspace).unwrap();
    let earlier = store.put(b"the picture the note showed first").unwrap();
    let current = store.put(b"the picture it shows now").unwrap();
    let dropped = store.put(b"an upload no record ever named").unwrap();

    // The note named one picture, then another: both versions are the
    // record's history.
    let governor = workspaces.serving_certificate().clone();
    for picture in [earlier.clone(), format!("tilo.content:{current}")] {
        workspaces
            .author_workspace_record(&hosted.workspace, &governor, (OPENED, ""), |doc| {
                doc.put(ROOT, "picture", picture.as_str())
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap();
    }

    let collected = tilo_relay::content::collection::collect(
        &workspaces,
        &stores,
        &hosted.workspace,
        SystemTime::UNIX_EPOCH,
    )
    .await
    .expect("every record of the workspace reads whole");
    assert!(
        collected.hashes.is_empty(),
        "content written within the grace is kept, named or not"
    );

    let later = SystemTime::now() + std::time::Duration::from_secs(60);
    let collected =
        tilo_relay::content::collection::collect(&workspaces, &stores, &hosted.workspace, later)
            .await
            .expect("every record of the workspace reads whole");
    assert_eq!(collected.hashes, std::slice::from_ref(&dropped));
    assert!(
        store.has(&earlier),
        "a version the note can go back to keeps its picture"
    );
    assert!(store.has(&current), "the picture the note shows stays");
    assert!(!store.has(&dropped), "content nothing names is let go of");
    relay.shutdown().await;
}

/// A Viewer writes nothing of the workspace's, but their own record of an app
/// (of a kind its manifest declares each member's own, their settings) is theirs whatever their role: the
/// relay keeps their change of it and their other device takes it in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_writes_their_own_record() {
    Box::pin(a_viewer_writes_their_own_record_of(
        "so.tilo.settings",
        "settings.root",
    ))
    .await;
}

/// A person's mail is theirs: a Viewer keeps their own mailbox record,
/// which their other device takes in, on a
/// fresh workspace no editor ever opened Mail in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_keeps_their_own_mailbox() {
    Box::pin(a_viewer_writes_their_own_record_of(
        "so.tilo.mail",
        "mail.mailbox",
    ))
    .await;
}

async fn a_viewer_writes_their_own_record_of(own_app: &str, kind: &str) {
    let alice = TestUser::fresh("Fixture governor");
    let carol = TestUser::fresh("Fixture viewer");
    let host = alice.enrol("own-host", DeviceCapabilities::member());
    let laptop = alice.enrol("own-laptop", DeviceCapabilities::primary());
    let viewer = carol.enrol("own-viewer", DeviceCapabilities::member());
    let phone = carol.enrol("own-viewer-phone", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let relay = start(scratch.path(), &alice, &host).await;
    let hosted = create_and_enter(&relay, &laptop, "A viewer's own record").await;
    let workspace = hosted.workspace_id;
    admit_viewer(&relay, &host, &viewer, &hosted.workspace).await;
    let roster = vec![
        host.cert.clone(),
        laptop.cert.clone(),
        viewer.cert.clone(),
        phone.cert.clone(),
    ];
    let replica = || {
        let mut peer = StatePeer::new(
            &hosted.workspace,
            host.cert.clone(),
            roster.clone(),
            RevocationList::new(),
            SystemTime::now(),
        )
        .unwrap();
        peer.select_exact_admissions().unwrap();
        peer
    };
    let own = collab_types::OwnRecord::new(kind, carol.user_pk)
        .unwrap()
        .record_id();
    // The workspace's owner is live while Carol writes: a member like any other
    // for her own record, which never reaches the owner's device.
    let owner_session = session(&relay, &laptop, workspace).await;
    let viewer_session = session(&relay, &viewer, workspace).await;
    let phone_session = session(&relay, &phone, workspace).await;
    let mut viewer_peer = replica();
    let mut phone_peer = replica();
    for (session, peer) in [
        (&viewer_session, &mut viewer_peer),
        (&phone_session, &mut phone_peer),
    ] {
        receive_until(
            "the viewer's device holds the workspace",
            session,
            peer,
            &mut Vec::new(),
            |peer, _| peer.admitted_workspace().is_some(),
        )
        .await;
    }
    crate::common::wait_until_subscribed(&relay, workspace, &phone.device_pubkey()).await;

    let created = viewer_peer
        .publication_mut()
        .publish_document(
            own_app,
            &own,
            AutoCommit::new().save(),
            &viewer.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&viewer_session, &created);
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "theme", "dark").unwrap();
    doc.commit_edit("fixture", "theme");
    let chosen = viewer_peer
        .publication_mut()
        .publish_incremental(
            own_app,
            &own,
            doc.save_after(&[]),
            &viewer.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&viewer_session, &chosen);
    receive_until(
        "the viewer's other device takes in their own record",
        &phone_session,
        &mut phone_peer,
        &mut Vec::new(),
        |peer, _| {
            let Some(doc) = peer
                .document(own_app, &own)
                .and_then(|bytes| AutoCommit::load(&bytes).ok())
            else {
                return false;
            };
            doc.get(ROOT, "theme")
                .ok()
                .flatten()
                .and_then(|(value, _)| value.into_string().ok())
                .as_deref()
                == Some("dark")
        },
    )
    .await;
    assert!(
        matches!(
            answer_to_opening(&phone_session, own_app, &own).await,
            tilo_relay_protocol::RecordAnswer::Sent { frames } if frames > 0
        ),
        "her other device opens her own record"
    );
    assert_eq!(
        answer_to_opening(&owner_session, own_app, &own).await,
        tilo_relay_protocol::RecordAnswer::NotYours,
        "the owner's device was sent none of it live, and opening it is answered as hers"
    );
    // Joined again, the owner's device is replayed the workspace without it.
    owner_session.close().await;
    let owner_session = session(&relay, &laptop, workspace).await;
    assert_eq!(
        answer_to_opening(&owner_session, own_app, &own).await,
        tilo_relay_protocol::RecordAnswer::NotYours,
        "nor is it in the owner's catch-up"
    );
    owner_session.close().await;
    // Her phone is away while she writes her own record again: it takes the
    // write in when it comes back.
    phone_session.close().await;
    let heads = doc.get_heads();
    doc.put(ROOT, "theme", "light").unwrap();
    doc.commit_edit("fixture", "theme");
    let away = viewer_peer
        .publication_mut()
        .publish_incremental(
            own_app,
            &own,
            doc.save_after(&heads),
            &viewer.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&viewer_session, &away);
    let phone_session = session(&relay, &phone, workspace).await;
    receive_until(
        "her phone, back, takes in the write made while it was away",
        &phone_session,
        &mut phone_peer,
        &mut Vec::new(),
        |peer, _| theme(peer, own_app, &own).as_deref() == Some("light"),
    )
    .await;

    // The workspace's owner writes into Carol's own record anyway, as a
    // modified page could: built from a replica that holds it, the write names
    // Carol as the record's person and is signed by the owner's device. The
    // host neither keeps nor forwards it; her phone would refuse it loudly
    // (`receive_until` unwraps every delta it applies), and only her own
    // later write reaches it.
    let owner_session = session(&relay, &laptop, workspace).await;
    let heads = doc.get_heads();
    doc.put(ROOT, "theme", "the owner's").unwrap();
    doc.commit_edit("fixture", "theme");
    let mut forged = viewer_peer
        .clone()
        .publication_mut()
        .publish_incremental(
            own_app,
            &own,
            doc.save_after(&heads),
            &viewer.sk,
            SystemTime::now(),
        )
        .unwrap();
    let NeighbourMsg::AppDocument {
        peer,
        envelope,
        own: named,
        ..
    } = &mut forged
    else {
        panic!("a write of a record is an app document");
    };
    assert_eq!(
        named.as_ref().map(|own| own.owner),
        Some(carol.user_pk),
        "the write names the record's person"
    );
    *peer = tilo_peer_state::peer_handle(&laptop.device_pubkey());
    envelope.author = collab_types::Ed25519PublicKey::from_bytes(laptop.device_pubkey());
    envelope.seq = 1;
    collab_types::sign_op(envelope, &laptop.sk).unwrap();
    publish(&owner_session, &forged);
    // Past the time the host takes a write in, so a forwarded forgery would
    // arrive before Carol's write below.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    owner_session.close().await;
    let mut doc = AutoCommit::load(&viewer_peer.document(own_app, &own).unwrap()).unwrap();
    let heads = doc.get_heads();
    doc.put(ROOT, "theme", "blue").unwrap();
    doc.commit_edit("fixture", "theme");
    let blue = viewer_peer
        .publication_mut()
        .publish_incremental(
            own_app,
            &own,
            doc.save_after(&heads),
            &viewer.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&viewer_session, &blue);
    receive_until(
        "her phone takes in her write and nothing of the owner's",
        &phone_session,
        &mut phone_peer,
        &mut Vec::new(),
        |peer, _| theme(peer, own_app, &own).as_deref() == Some("blue"),
    )
    .await;

    // Nothing of the owner's own record was written yet: opening it named as
    // hers, Carol's device is told it is not hers, and the owner's device that
    // it holds none.
    let alices = tilo_relay_protocol::OwnedChannel {
        app_id: own_app.to_owned(),
        record: collab_types::OwnRecord::new(kind, alice.user_pk).unwrap(),
    };
    assert_eq!(
        answer_to_opening_own(&viewer_session, &alices).await,
        tilo_relay_protocol::RecordAnswer::NotYours,
        "another member's own record is not hers, written or not"
    );
    let owner_session = session(&relay, &laptop, workspace).await;
    assert_eq!(
        answer_to_opening_own(&owner_session, &alices).await,
        tilo_relay_protocol::RecordAnswer::Sent { frames: 0 },
        "the owner's own record, of which nothing is written yet"
    );
    owner_session.close().await;

    // A write of her own record the host does not retain (sent as it is, not
    // as a retained change) waits for her phone while it is away, and for no
    // other member's device.
    phone_session.close().await;
    viewer_session.close().await;
    let heads = doc.get_heads();
    doc.put(ROOT, "theme", "queued").unwrap();
    doc.commit_edit("fixture", "theme");
    let unretained = viewer_peer
        .publication_mut()
        .publish_incremental(
            own_app,
            &own,
            doc.save_after(&heads),
            &viewer.sk,
            SystemTime::now(),
        )
        .unwrap();
    assert!(
        matches!(&unretained, NeighbourMsg::AppDocument { own: Some(said), .. } if said.owner == carol.user_pk),
        "her write names her own record on the wire"
    );
    let raw = RelayClient::connect_to_pinned_host(
        device_endpoint(&viewer).await,
        relay.node_addr(),
        viewer.cert.clone(),
        viewer.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    raw.send(Envelope::Subscribe {
        workspace_id: workspace,
    })
    .unwrap();
    tilo_test_env::within("the viewer's connection is subscribed", async {
        while !matches!(raw.recv().await, Some(Envelope::Ready { .. })) {}
    })
    .await;
    let sent = postcard::to_stdvec(&unretained).unwrap();
    raw.send(Envelope::Delta {
        workspace_id: workspace,
        author_device_public_key: viewer.device_pubkey(),
        authored_at_ns: 1,
        delta: sent.clone(),
        signature: [0; 64],
    })
    .unwrap();
    let pending = |device: &TestDevice| {
        let dir = scratch
            .path()
            .join("workspaces")
            .join(workspace.to_string())
            .join("pending")
            .join(hex::encode(device.device_pubkey()));
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".delta"))
                .count()
        })
    };
    tilo_test_env::within("the write is queued for her phone", async {
        while pending(&phone) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    assert_eq!(pending(&laptop), 0, "nor for the owner's device");
    let back = RelayClient::connect_to_pinned_host(
        device_endpoint(&phone).await,
        relay.node_addr(),
        phone.cert.clone(),
        phone.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    back.send(Envelope::Subscribe {
        workspace_id: workspace,
    })
    .unwrap();
    tilo_test_env::within(
        "her phone, back, is sent the write kept for it alone",
        async {
            loop {
                match back.recv().await.expect("the connection stays open") {
                    Envelope::Delta { delta, .. } if delta == sent => return,
                    _ => {}
                }
            }
        },
    )
    .await;
    relay.shutdown().await;
}

/// [`answer_to_opening`] for a member's own record named with its kind and
/// person, which the host answers even before anything of it was written.
async fn answer_to_opening_own(
    session: &RelayCoeditSession,
    own: &tilo_relay_protocol::OwnedChannel,
) -> tilo_relay_protocol::RecordAnswer {
    let channel = neighbour_wire::app_document_channel(&own.app_id, &own.record.record_id());
    session.open_own_record(own.clone()).unwrap();
    tilo_test_env::within("the host answers the opened record", async {
        loop {
            if let Inbound::RecordOpened {
                channel: opened,
                answer,
            } = session.recv().await.expect("the session stays open")
                && opened == channel
            {
                return answer;
            }
        }
    })
    .await
}

fn theme(peer: &StatePeer, app: &str, record: &str) -> Option<String> {
    AutoCommit::load(&peer.document(app, record)?)
        .ok()?
        .get(ROOT, "theme")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// What the host answers `session` when it opens `app`'s record `record`. Every
/// frame that arrives first is checked: none may be a change of `record`
/// unless the answer says the record was sent.
async fn answer_to_opening(
    session: &RelayCoeditSession,
    app: &str,
    record: &str,
) -> tilo_relay_protocol::RecordAnswer {
    let channel = neighbour_wire::app_document_channel(app, record);
    session.open_record(channel).unwrap();
    let mut received = false;
    let answer = tilo_test_env::within("the host answers the opened record", async {
        loop {
            match session.recv().await.expect("the session stays open") {
                Inbound::Delta { data, .. } => {
                    if let Ok(NeighbourMsg::AppDocument { record_id, .. }) =
                        postcard::from_bytes::<NeighbourMsg>(&data)
                        && record_id == record
                    {
                        received = true;
                    }
                }
                Inbound::RecordOpened {
                    channel: opened,
                    answer,
                } if opened == channel => return answer,
                _ => {}
            }
        }
    })
    .await;
    if answer == tilo_relay_protocol::RecordAnswer::NotYours {
        assert!(
            !received,
            "a device that may not open the record received it"
        );
    }
    answer
}
