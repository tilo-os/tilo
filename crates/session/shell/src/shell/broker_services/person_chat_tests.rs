use super::NativeOwnerCommitService;
use tilo_action_broker::services::person_chat::PersonChatServices;

// Production builds the service without waiting for disk on the compositor
// loop. The fixture waits through the same owner source on this worker thread.
fn opened_service(
    tree: crate::shell::state::WorkspaceTreeHandle,
    network: Option<std::sync::Arc<crate::shell::network::ShellNetwork>>,
) -> NativeOwnerCommitService {
    let service = NativeOwnerCommitService::new(tree, network);
    service.resolve_owner().unwrap();
    service
}

#[test]
fn snapshot_bounds_apply_before_appending_pages_or_expanding_reply() {
    use std::io::Write;
    assert!(super::check_snapshot_bound(100, 512 * 1024 + 1).is_err());
    assert!(super::check_snapshot_bound(2001, 1).is_err());
    assert!(super::check_snapshot_bound(2000, 512 * 1024).is_ok());
    let mut reply = super::BoundedReply(Vec::new());
    let oversized = vec![0; 1024 * 1024 + 1];
    assert!(reply.write_all(&oversized).is_err());
    assert!(
        reply.0.is_empty(),
        "reject before extending the reply allocation"
    );
}

#[test]
fn person_bridge_refuses_replica_fallback_without_private_transport() {
    let (_directory, owner, room, _, _) =
        super::super::super::native_workspace_owner::test_enrolled_owner();
    let certificate = owner
        .with_current(room, |view| {
            assert_ne!(view.device(), view.governor(), "fixture must be a replica");
            Ok(view.device().clone())
        })
        .unwrap();
    let service = opened_service(
        super::super::super::native_workspace_owner::tree_of(&owner),
        None,
    );
    assert!(
        service
            .command(
                &room.to_string(),
                &certificate,
                br#"{"operation":"contacts"}"#
            )
            .is_err()
    );
    assert!(
        service
            .subscribe(
                &room.to_string(),
                &certificate,
                &hex::encode(certificate.user_public_key)
            )
            .is_err()
    );
    drop(service);
}

#[test]
fn person_bridge_uses_actual_private_owner_and_survives_restart() {
    let (directory, owner, room, certificate) = private_owner_fixture();
    let tree = super::super::super::native_workspace_owner::tree_of(&owner);
    let service = opened_service(tree, None);
    let contacts: serde_json::Value = serde_json::from_slice(
        &service
            .command(
                &room.to_string(),
                &certificate,
                br#"{"operation":"contacts"}"#,
            )
            .unwrap(),
    )
    .unwrap();
    let peer = contacts
        .as_array()
        .unwrap()
        .first()
        .expect("actual certified workspace peer")["public_key"]
        .as_str()
        .unwrap();
    assert_eq!(peer.len(), 64);
    let execute = |command: serde_json::Value| -> Result<serde_json::Value, String> {
        let reply = service.command(
            &room.to_string(),
            &certificate,
            &serde_json::to_vec(&command).unwrap(),
        )?;
        serde_json::from_slice(&reply).map_err(|error| error.to_string())
    };
    let public_before = owner
        .with_current(room, |view| Ok(view.peer().document("so.tilo.chat", "")))
        .unwrap();
    let opened = execute(serde_json::json!({"operation":"open","public_key":peer})).unwrap();
    assert_eq!(opened["peer"], peer);
    let invocation = uuid::Uuid::new_v4();
    let command = serde_json::json!({"operation":"send","public_key":peer,"text":"Private owner bridge body","invocation":invocation});
    let saved = execute(command.clone()).unwrap();
    assert_eq!(
        saved["messages"][0]["author"],
        hex::encode(certificate.user_public_key)
    );
    assert_eq!(saved["messages"][0]["text"], "Private owner bridge body");
    assert_eq!(
        execute(command).unwrap(),
        saved,
        "retry retains the actual persisted message ID"
    );
    assert_eq!(
        owner
            .with_current(room, |view| Ok(view.peer().document("so.tilo.chat", "")))
            .unwrap(),
        public_before
    );
    assert!(execute(serde_json::json!({"operation":"send","public_key":peer,"text":"changed","invocation":invocation})).is_err());
    assert!(execute(serde_json::json!({"operation":"read","public_key":"Person"})).is_err());
    let mut forged = certificate.clone();
    forged.device_public_key = [0; 32];
    assert!(
        service
            .command(&room.to_string(), &forged, br#"{"operation":"contacts"}"#)
            .is_err()
    );
    drop(service);
    drop(owner);
    let path = directory.path().join("state.json");
    let mut reopened = super::super::super::persist::load_tree_from(&path).unwrap();
    reopened.persist_path = Some(path);
    let service = opened_service(reopened.shared(), None);
    let loaded: serde_json::Value = serde_json::from_slice(
        &service
            .command(
                &room.to_string(),
                &certificate,
                &serde_json::to_vec(&serde_json::json!({"operation":"read","public_key":peer}))
                    .unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    drop(service);
    assert_eq!(loaded, saved);
}

#[test]
fn person_bridge_never_acknowledges_failed_private_persistence() {
    let (directory, owner, room, certificate) = private_owner_fixture();
    let service = opened_service(
        super::super::super::native_workspace_owner::tree_of(&owner),
        None,
    );
    let contacts: serde_json::Value = serde_json::from_slice(
        &service
            .command(
                &room.to_string(),
                &certificate,
                br#"{"operation":"contacts"}"#,
            )
            .unwrap(),
    )
    .unwrap();
    let peer = contacts
        .as_array()
        .unwrap()
        .first()
        .expect("actual certified workspace peer")["public_key"]
        .as_str()
        .unwrap();
    service
        .command(
            &room.to_string(),
            &certificate,
            &serde_json::to_vec(&serde_json::json!({"operation":"open","public_key":peer}))
                .unwrap(),
        )
        .unwrap();
    let path = directory.path().join("state.json");
    let before = std::fs::read(&path).unwrap();
    let command = serde_json::to_vec(&serde_json::json!({"operation":"send","public_key":peer,
        "text":"This failed private write must not be acknowledged", "invocation":uuid::Uuid::new_v4()})).unwrap();
    let result = tilo_files::native::atomic::test_support::with_failure(
        tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
        || service.command(&room.to_string(), &certificate, &command),
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
    let read: serde_json::Value = serde_json::from_slice(
        &service
            .command(
                &room.to_string(),
                &certificate,
                &serde_json::to_vec(&serde_json::json!({"operation":"read","public_key":peer}))
                    .unwrap(),
            )
            .unwrap(),
    )
    .unwrap();
    drop(service);
    assert!(read["messages"].as_array().unwrap().is_empty());
}

fn private_owner_fixture() -> (
    tempfile::TempDir,
    tilo_joined_room::NativeWorkspaceOwner,
    crate::shell::state::RoomId,
    tilo_user_identity::DeviceCert,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(directory.path().join("state.json"));
    let owner = crate::shell::native_workspace_owner::open(tree.shared()).unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
    let certificate = tilo_joined_room::testing::certificate(
        &ed25519_dalek::SigningKey::from_bytes(&[62; 32]),
        &key,
        "Private owner host",
    );
    let mut governor =
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
    let room = crate::shell::state::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    enroll_contact(&owner, &mut governor, room, std::time::SystemTime::now());
    (directory, owner, room, certificate)
}
fn enroll_contact(
    owner: &tilo_joined_room::NativeWorkspaceOwner,
    governor: &mut tilo_joined_room::testing::governor::TestGovernor,
    room: tilo_desktop_model::RoomId,
    now: std::time::SystemTime,
) {
    use tilo_peer_state::join;
    let key = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
    let certificate = tilo_joined_room::testing::certificate(
        &ed25519_dalek::SigningKey::from_bytes(&[74; 32]),
        &key,
        "Conversation peer",
    );
    let ns = tilo_joined_room::testing::ns(now).unwrap();
    let hint = governor.hint(iroh::EndpointAddr::new(
        iroh::EndpointId::from_bytes(&governor.governor().device_public_key).unwrap(),
    ));
    let invitation = governor.issue(
        vec![hint],
        join::MemberRole::Editor,
        Some(certificate.user_public_key),
        Some("Conversation peer".into()),
        ns.checked_add(60_000_000_000).unwrap(),
    );
    let response = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id: [75; 32],
        }),
    );
    let join::JoinTransportResponse::Challenge(challenge) = response else {
        panic!("certified contact challenge refused");
    };
    let response = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Complete(join::JoinCompleteRequest {
            proof: join::sign_join_proof(challenge.challenge, &key).unwrap(),
        }),
    );
    assert!(matches!(response, join::JoinTransportResponse::Complete(_)));
    for (bytes, _) in governor.outgoing() {
        let message: neighbour_wire::NeighbourMsg = postcard::from_bytes(bytes).unwrap();
        owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    governor.governor().device_public_key,
                ),
                message,
                now,
            )
            .unwrap();
    }
    assert!(
        owner
            .direct_contacts(
                room,
                &tilo_peer_state::caller_authority::CallerAuthority::Person {
                    caller: governor.governor().clone(),
                },
                now
            )
            .unwrap()
            .iter()
            .any(|contact| contact.public_key == hex::encode(certificate.user_public_key))
    );
}
