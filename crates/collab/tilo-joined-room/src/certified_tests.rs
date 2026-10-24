use super::*;
use ed25519_dalek::SigningKey;
use std::time::{Duration, SystemTime};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_peer_state::join::{self, InviteGrant, JoinChallenge, JoinCompleteRequest, MemberRole};
use tilo_peer_state::{SeedState, StatePeer, seed_room};
use tilo_user_identity::{DeviceCapabilities, RevocationList, sign_device_cert};

fn certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn actual_redemption() -> (
    VerifiedJoin,
    JoinCompleteRequest,
    JoinBootstrap,
    DeviceCert,
    TransportHint,
    SystemTime,
) {
    actual_redemption_at(SystemTime::now())
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear invite-to-bootstrap redemption fixture; splitting it would scatter the \
              protocol steps each assertion depends on"
)]
fn actual_redemption_at(
    now: SystemTime,
) -> (
    VerifiedJoin,
    JoinCompleteRequest,
    JoinBootstrap,
    DeviceCert,
    TransportHint,
    SystemTime,
) {
    let ns = u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let governor_key = SigningKey::from_bytes(&[71; 32]);
    let governor = certificate(
        &SigningKey::from_bytes(&[72; 32]),
        &governor_key,
        "Governor",
    );
    let device_key = SigningKey::from_bytes(&[73; 32]);
    let device = certificate(
        &SigningKey::from_bytes(&[74; 32]),
        &device_key,
        "Joining device",
    );
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let workspace_id = tilo_relay_protocol::room_workspace_id(room).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: room.into(),
        workspace_id,
        governing_authority: governor.clone(),
        endpoint_addr: iroh_base::EndpointAddr::new(
            iroh_base::EndpointId::from_bytes(&governor.device_public_key).unwrap(),
        ),
        host_user_public_key: governor.user_public_key,
        host_device_certificate: governor.clone(),
        packet_relay_url: "https://relay.invalid".into(),
    }
    .transport_hint(&governor_key)
    .unwrap();
    let signed = join::issue_invitation(
        InviteGrant {
            workspace_id,
            room: room.into(),
            authority: governor.clone(),
            issuer: governor.clone(),
            delegation: None,
            role: MemberRole::Editor,
            nonce: [75; 16],
            recipient_user_public_key: Some(device.user_public_key),
            invitee_name: Some("Guest".into()),
            expires_at_ns: ns.checked_add(60_000_000_000).unwrap(),
            transport: vec![hint.clone()],
        },
        &governor_key,
    )
    .unwrap();
    let join = join::decode_and_verify_invitation(
        &signed,
        Some(device.user_public_key),
        &RevocationList::new(),
        ns,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &device,
        [76; 32],
        [77; 32],
        ns.checked_add(30_000_000_000).unwrap(),
        ns,
    )
    .unwrap();
    let proof = join::sign_join_proof(challenge.clone(), &device_key).unwrap();
    let redemption =
        join::verify_join_proof(&proof, &challenge, &device, &RevocationList::new(), ns).unwrap();
    let mut peer = StatePeer::new(
        room,
        governor.clone(),
        vec![governor.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(room, &governor, std::slice::from_ref(&governor)).unwrap(),
            documents: vec![],
        },
        &governor_key,
        now,
    )
    .unwrap();
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redemption, &governor_key, now)
            .unwrap()
    );
    let bootstrap = JoinBootstrap {
        version: 1,
        invite_id: join.invite_id(),
        device_cert: device.clone(),
        role: MemberRole::Editor,
        room: room.into(),
        workspace_id,
        authority: governor,
        certified_roster: peer.certificates(),
        revocations: peer.revocations(),
        records: peer.rows(),
        governance_messages: vec![postcard::to_stdvec(&peer.room_message()).unwrap()],
        generation: 1,
    };
    (
        join,
        JoinCompleteRequest { proof },
        bootstrap,
        device,
        hint,
        now,
    )
}

#[test]
fn native_enrollment_retains_actual_verified_redemption_and_governance() {
    let (join, complete, bootstrap, device, hint, now) = actual_redemption();
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        join.clone(),
        complete,
        device.clone(),
        hint.clone(),
        now,
    )
    .unwrap();
    let (_directory, owner) = pending_owner();
    let id = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let enrolled = crate::testing::finalize(intent, id, bootstrap.clone()).unwrap();
    assert_eq!(enrolled.verified_join(), &join);
    assert_eq!(enrolled.bootstrap(), &bootstrap);
    assert_eq!(enrolled.local_device(), &device);
    assert_eq!(enrolled.transport_hint(), &hint);
    assert!(
        enrolled.delivery_hint().is_none(),
        "Iroh enrollment cannot invent a delivery host"
    );
}

#[test]
fn native_enrollment_refuses_bootstrap_scope_and_identity_substitution() {
    for change in 0..6 {
        let (join, complete, mut bootstrap, device, hint, now) = actual_redemption();
        match change {
            0 => bootstrap.invite_id[0] ^= 1,
            1 => bootstrap.workspace_id ^= 1,
            2 => bootstrap.room = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
            3 => bootstrap.device_cert = bootstrap.authority.clone(),
            4 => bootstrap.role = MemberRole::Viewer,
            _ => bootstrap.generation = 0,
        }
        let intent =
            VerifiedNativeEnrollmentIntent::from_proof(join, complete, device, hint, now).unwrap();
        let (_directory, owner) = pending_owner();
        let id = owner.prepare_enrollment_into(&intent, None, now).unwrap();
        assert!(crate::testing::finalize(intent, id, bootstrap).is_err());
    }
}

fn pending_owner() -> (tempfile::TempDir, crate::NativeWorkspaceOwner) {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let owner = crate::open_file(&directory.path().join("joined-rooms.json")).unwrap();
    (directory, owner)
}

#[test]
fn native_enrollment_retains_exact_complete_bytes_and_rejects_tampering() {
    let (join, complete, _, device, hint, now) = actual_redemption();
    let exact = postcard::to_stdvec(&JoinTransportRequest::Complete(complete.clone())).unwrap();
    let intent =
        VerifiedNativeEnrollmentIntent::from_proof(join, complete, device, hint, now).unwrap();
    let saved = intent.durable_bytes().unwrap();
    let restored = VerifiedNativeEnrollmentIntent::restore(&saved).unwrap();
    assert_eq!(restored.complete_bytes(), exact);
    assert_eq!(restored.proof_digest(), intent.proof_digest());
    let (_directory, owner) = pending_owner();
    let id = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let stored = owner.pending_enrollment(intent.room()).unwrap().unwrap();
    assert_eq!(stored.id(), &id);
    assert_eq!(stored.intent_bytes(), saved);
    let mut record: IntentRecord = postcard::from_bytes(&saved).unwrap();
    let mut request: JoinTransportRequest = postcard::from_bytes(&record.complete).unwrap();
    if let JoinTransportRequest::Complete(value) = &mut request {
        value.proof.signature[0] ^= 1;
    }
    record.complete = postcard::to_stdvec(&request).unwrap();
    assert!(
        VerifiedNativeEnrollmentIntent::restore(&postcard::to_stdvec(&record).unwrap()).is_err()
    );
}

#[test]
fn native_enrollment_uncertain_complete_survives_challenge_expiry_exactly() {
    let prepared = SystemTime::now() - Duration::from_secs(120);
    let (join, complete, _, device, hint, _) = actual_redemption_at(prepared);
    assert!(complete.proof.challenge.expires_at_ns < now_ns(SystemTime::now()).unwrap());
    let intent =
        VerifiedNativeEnrollmentIntent::from_proof(join, complete, device, hint, prepared).unwrap();
    let saved = intent.durable_bytes().unwrap();
    let restored = VerifiedNativeEnrollmentIntent::restore(&saved).unwrap();
    assert_eq!(restored.complete_bytes(), intent.complete_bytes());
    assert_eq!(restored.durable_bytes().unwrap(), saved);
}
