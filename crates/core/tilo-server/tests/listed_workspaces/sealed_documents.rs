//! A member seals a write to a record the workspace's host holds, as every
//! member of a workspace whose channels a node takes part in does. The host keeps
//! the ciphertext as the record's history and goes on governing: the next
//! person is still let in, and so is one after the server restarts.
use super::*;

/// `person`'s `device` redeems an editor's invitation to `workspace` that `issuer` issues, and
/// links to the workspace from what its join alone gave it: the link brings the rest.
pub(super) fn joined<'a>(
    server: &'a Server,
    issuer: &'a TestDevice,
    workspace: &'a WorkspaceListing,
    person: &'a TestUser,
    device: &'a TestDevice,
    request_id: [u8; 32],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Linked, String>> + Send + 'a>> {
    Box::pin(async move {
        let invitation = invite(
            server,
            issuer,
            &workspace.workspace,
            Role::Editor,
            person.user_pk,
        )
        .await;
        let verified = decode_and_verify_invitation(
            &invitation.signed_invitation,
            None,
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        let admission = WorkspaceJoinClient::new(device_endpoint(device).await)
            .redeem_hint(
                &verified.grant().transport[0],
                invitation.signed_invitation.clone(),
                device.cert.clone(),
                &device.sk,
                request_id,
            )
            .await
            .map_err(|error| error.to_string())?;
        let bootstrap = admission.bootstrap;
        let mut peer = StatePeer::new(
            &bootstrap.workspace_id.to_string(),
            bootstrap.host.clone(),
            bootstrap.certified_roster.clone(),
            RevocationList::new(),
            SystemTime::now(),
        )?;
        peer.select_exact_admissions()
            .map_err(|error| error.to_string())?;
        for bytes in &bootstrap.governance_messages {
            peer.receive_from(
                TransportOrigin::RetainedServerDelta(bootstrap.host.device_public_key),
                postcard::from_bytes(bytes).map_err(|error| error.to_string())?,
                SystemTime::now(),
            )
            .map_err(|error| error.to_string())?;
        }
        Linked::open_seeded(server, device, workspace.workspace_id, peer)
            .await
            .map_err(|refusal| refusal.to_string())
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sealed_write_to_a_shared_record_never_keeps_the_next_person_out() {
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let workspace = create_and_enter(&server, &laptop, "Lounge").await;
    let bob_link = joined(&server, &laptop, &workspace, &bob, &phone, [7; 32])
        .await
        .expect("the workspace admits Bob's phone");
    let alice_link = Linked::open(&server, &laptop, workspace.workspace_id)
        .await
        .expect("the workspace admits Alice's laptop");
    crate::common::receives(&alice_link, |peer| {
        role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor)
    })
    .await;
    crate::common::wait_until_subscribed(&server, workspace.workspace_id, &phone.device_pubkey())
        .await;

    // Alice's first write makes the record, and the host holds it.
    alice_link
        .publish(|peer| {
            Ok(write(
                peer,
                "topic",
                "welcome",
                &laptop.sk,
                &laptop.cert.user_public_key,
            ))
        })
        .expect("Alice's write goes out on her link");
    crate::common::receives(&bob_link, |peer| {
        text(peer, "topic").as_deref() == Some("welcome")
    })
    .await;

    // Bob's write leaves his device sealed; the host takes it in.
    let workspaces = server.workspaces().unwrap();
    let mut followed = workspaces.document_writes();
    bob_link
        .publish(|peer| {
            Ok(write(
                peer,
                "reply",
                "hi",
                &phone.sk,
                &phone.cert.user_public_key,
            ))
        })
        .expect("Bob's sealed write goes out on his link");
    tilo_test_env::within("the host follows Bob's sealed write", async {
        while followed.recv().await.unwrap().app != APP {}
    })
    .await;

    // A page's next write is sealed the same way, and the host follows it too.
    bob_link
        .publish(|peer| {
            Ok(write(
                peer,
                "reply",
                "again",
                &phone.sk,
                &phone.cert.user_public_key,
            ))
        })
        .expect("Bob's next sealed write goes out on his link");
    tilo_test_env::within("the host follows Bob's sealed write again", async {
        while followed.recv().await.unwrap().app != APP {}
    })
    .await;

    // The next person is let in, by a governance change published after it.
    let carol = TestUser::fresh("Carol");
    let tablet = carol.enrol("carol-tablet", DeviceCapabilities::member());
    let tablet_link = joined(&server, &laptop, &workspace, &carol, &tablet, [8; 32])
        .await
        .expect("a sealed record write keeps nobody out");
    let is_member = |members: &[tilo_peer_state::Member], person: [u8; 32]| {
        members
            .iter()
            .any(|member| member.root() == Some(person) && !member.removed)
    };
    assert!(is_member(
        &workspaces
            .summary(&workspace.workspace)
            .await
            .unwrap()
            .members,
        carol.user_pk
    ));

    // Reopened, the host replays the sealed write and still lets people in.
    drop(tablet_link);
    drop(bob_link);
    drop(alice_link);
    server.shutdown().await;
    let server = start(state.path(), &alice, &host).await;
    let dan = TestUser::fresh("Dan");
    let watch = dan.enrol("dan-watch", DeviceCapabilities::member());
    let watch_link = joined(&server, &laptop, &workspace, &dan, &watch, [9; 32])
        .await
        .expect("a reopened workspace with a sealed record keeps nobody out");
    assert!(is_member(
        &server
            .workspaces()
            .unwrap()
            .summary(&workspace.workspace)
            .await
            .unwrap()
            .members,
        dan.user_pk
    ));
    drop(watch_link);
    server.shutdown().await;
}
