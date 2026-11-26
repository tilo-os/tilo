//! Opening an app for the first time in a workspace a home relay hosts, after
//! the workspace lets the app author: a member's device signs the app's empty
//! record itself, as every copy admits a member's record, and the app's
//! first state reaches the record as the member's own change.
use super::*;

const OPENED: &str = "so.tilo.notes";

/// `peer`'s empty record of `app`, sealed under the members' keys and signed with `key`, to
/// publish.
fn genesis(
    peer: &mut StatePeer,
    app: &str,
    key: &ed25519_dalek::SigningKey,
) -> Result<NeighbourMsg, String> {
    sealed_checkpoint(peer, &KEYS, key, (app, ""), AutoCommit::new().save())
}

/// The person whose certified device `key` is in `peer`'s workspace.
fn person(peer: &StatePeer, key: &ed25519_dalek::SigningKey) -> [u8; 32] {
    let device = key.verifying_key().to_bytes();
    peer.certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == device)
        .expect("the workspace holds the device's certificate")
        .user_public_key
}

fn first_state(peer: &StatePeer) -> Option<String> {
    let doc = AutoCommit::load(&opened(peer, &KEYS, (OPENED, ""))?).ok()?;
    doc.get(ROOT, "title")
        .ok()
        .flatten()
        .and_then(|(value, _)| value.into_string().ok())
}

/// Admit `member`'s person to `workspace` as a Viewer, through an invitation the
/// server issues and `member`'s device redeems.
async fn admit_viewer(server: &Server, host: &TestDevice, member: &TestDevice, workspace: &str) {
    let issued = server
        .workspaces()
        .unwrap()
        .issue(
            workspace,
            &host.cert,
            tilo_workspace_service::workspaces::IssueInvitationRequest {
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
    let alice = TestUser::fresh("Fixture owner");
    let bob = TestUser::fresh("Fixture editor");
    let carol = TestUser::fresh("Fixture viewer");
    let host = alice.enrol("private-host", DeviceCapabilities::member());
    let laptop = alice.enrol("private-laptop", DeviceCapabilities::primary());
    let member = bob.enrol("private-member", DeviceCapabilities::member());
    let viewer = carol.enrol("private-viewer", DeviceCapabilities::member());
    let scratch = tempfile::tempdir().unwrap();
    let server = start(scratch.path(), &alice, &host).await;
    let hosted = create_and_enter(&server, &laptop, "Private app records").await;
    let workspace = hosted.workspace_id;
    members::admit(&server, &host, &member, &hosted.workspace).await;
    admit_viewer(&server, &host, &viewer, &hosted.workspace).await;

    let member_link = Linked::open(&server, &member, workspace)
        .await
        .expect("the workspace admits the member");
    let laptop_link = Linked::open(&server, &laptop, workspace)
        .await
        .expect("the workspace admits the laptop");
    crate::keep::common::receives(&member_link, |peer| peer.admitted_workspace().is_some()).await;
    // An editor's own change of an app's record is theirs: the record's
    // genesis is theirs to start, and it is published only under an
    // application id.
    assert!(
        member_link
            .read(|peer| genesis(&mut peer.clone(), "so.tilo.notes\n", &member.sk))
            .is_err(),
        "a malformed application id is refused"
    );
    for malformed in ["", " ", "notes", "so..notes", ".so.tilo", "so.tilo.notes/x"] {
        assert!(
            member_link
                .read(|peer| genesis(&mut peer.clone(), malformed, &member.sk))
                .is_err(),
            "{malformed:?} is not an application id"
        );
    }
    crate::keep::common::wait_until_subscribed(&server, workspace, &member.device_pubkey()).await;
    member_link
        .publish(|peer| genesis(peer, OPENED, &member.sk))
        .expect("the member starts the record");
    crate::keep::common::receives(&laptop_link, |peer| {
        opened(peer, &KEYS, (OPENED, "")).is_some()
    })
    .await;
    let mut created = AutoCommit::load(&laptop_link.opened((OPENED, "")).unwrap()).unwrap();
    assert!(
        created.get_heads().is_empty(),
        "a member creates an empty record and nothing else"
    );
    assert!(
        member_link
            .read(|peer| genesis(&mut peer.clone(), OPENED, &member.sk))
            .is_err(),
        "a record the workspace holds is never created again"
    );

    // The app's first state is the member's own change onto the genesis.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "First note").unwrap();
    doc.commit_edit("fixture", "title", &member.cert.user_public_key);
    member_link
        .publish(|peer| sealed_changes(peer, &KEYS, &member.sk, (OPENED, ""), doc.save_after(&[])))
        .expect("the member's first state goes out on their link");
    crate::keep::common::receives(&laptop_link, |peer| {
        first_state(peer).as_deref() == Some("First note")
    })
    .await;

    // A viewer's device that skips its own checks and writes the record
    // anyway, as a modified page could: the server neither keeps nor
    // forwards the write. Another member's copy would refuse it loudly
    // (`receives` would see it land), and only the editor's later write reaches it.
    let viewer_link = Linked::open(&server, &viewer, workspace)
        .await
        .expect("the workspace admits the viewer");
    crate::keep::common::receives(&viewer_link, |peer| {
        first_state(peer).as_deref() == Some("First note")
    })
    .await;
    let retitle = |peer: &mut StatePeer, title: &str, key: &ed25519_dalek::SigningKey| {
        let person = person(peer, key);
        sealed_write(peer, &KEYS, key, (OPENED, ""), |doc| {
            doc.put(ROOT, "title", title).unwrap();
            doc.commit_edit("fixture", "title", &person);
        })
        .unwrap()
    };
    // The viewer's own copy refuses to sign the write, so the page
    // builds it from a member's copy and signs it as the viewer.
    let mut forged = member_link.read(|peer| {
        let mut copy = peer.clone();
        retitle(&mut copy, "A viewer's note", &member.sk)
    });
    if let NeighbourMsg::AppDocument { peer, envelope, .. } = &mut forged {
        *peer = tilo_peer_state::peer_handle(&viewer.device_pubkey());
        envelope.author = collab_types::Ed25519PublicKey::from_bytes(viewer.device_pubkey());
        envelope.seq = 1;
        collab_types::sign_op(envelope, &viewer.sk).unwrap();
    }
    viewer_link
        .send(forged)
        .expect("the forged write goes out on the viewer's link");
    // Past the time the server takes a write in, so a forwarded forgery
    // would arrive before the editor's write below.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    member_link
        .publish(|peer| Ok(retitle(peer, "Second note", &member.sk)))
        .expect("the editor's write goes out on their link");
    crate::keep::common::receives(&laptop_link, |peer| {
        first_state(peer).as_deref() == Some("Second note")
    })
    .await;
    server.shutdown().await;
}
