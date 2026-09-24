//! These tests use actual signed membership and native document admission.
use super::*;
use crate::network::invite::VerifiedNativeEnrollment;
use crate::shell::{persist, state::WorkspaceTree};
use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use qos_peer_state::{SeedDocument, SeedState, join, room::media, seed_room};
use std::collections::BTreeMap;
use tilo_relay::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "dev.qos.chat";

fn certificate(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

struct Fixture {
    enrollment: VerifiedNativeEnrollment,
    messages: Vec<NeighbourMsg>,
    governor: [u8; 32],
    now: SystemTime,
}
fn raw_fixture() -> (
    crate::network::invite::VerifiedNativeEnrollmentIntent,
    join::JoinBootstrap,
    Vec<NeighbourMsg>,
    [u8; 32],
    SystemTime,
) {
    raw_fixture_at(SystemTime::now())
}
fn raw_fixture_at(
    now: SystemTime,
) -> (
    crate::network::invite::VerifiedNativeEnrollmentIntent,
    join::JoinBootstrap,
    Vec<NeighbourMsg>,
    [u8; 32],
    SystemTime,
) {
    let ns = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let governor = certificate(
        &SigningKey::from_bytes(&[62; 32]),
        &governor_key,
        "Governor",
    );
    let device_key = SigningKey::from_bytes(&[63; 32]);
    let device = certificate(&SigningKey::from_bytes(&[64; 32]), &device_key, "Member");
    let host_key = SigningKey::from_bytes(&[69; 32]);
    let mut host = certificate(
        &SigningKey::from_bytes(&[62; 32]),
        &host_key,
        "Delivery host",
    );
    host.capabilities.can_act_as_host = true;
    sign_device_cert(&mut host, &SigningKey::from_bytes(&[62; 32])).unwrap();
    let locator = tilo_relay::workspace_join::DeliveryOriginLocatorV1 {
        version: 1,
        origin: "https://native.example".into(),
    }
    .transport_hint(&host, &host_key)
    .unwrap();
    let workspace_id = tilo_relay::room_workspace_id(ROOM).unwrap();
    let hint = IrohWorkspaceHostHintV1 {
        version: 1,
        mode: WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        room: ROOM.into(),
        workspace_id,
        governing_authority: governor.clone(),
        endpoint_addr: iroh::EndpointAddr::new(
            iroh::EndpointId::from_bytes(&governor.device_public_key).unwrap(),
        ),
        host_user_public_key: governor.user_public_key,
        host_device_certificate: governor.clone(),
        packet_relay_url: "https://relay.invalid".into(),
    }
    .transport_hint(&governor_key)
    .unwrap();
    let signed = join::issue_invitation(
        join::InviteGrant {
            workspace_id,
            room: ROOM.into(),
            authority: governor.clone(),
            issuer: governor.clone(),
            delegation: None,
            role: join::MemberRole::Editor,
            nonce: [65; 16],
            recipient_user_public_key: Some(device.user_public_key),
            invitee_name: Some("Member".into()),
            expires_at_ns: ns + 60_000_000_000,
            transport: vec![hint.clone(), locator],
        },
        &governor_key,
    )
    .unwrap();
    let verified = join::decode_and_verify_invitation(
        &signed,
        Some(device.user_public_key),
        &RevocationList::new(),
        ns,
    )
    .unwrap();
    let challenge = join::JoinChallenge::new(
        &verified,
        &device,
        [66; 32],
        [67; 32],
        ns + 30_000_000_000,
        ns,
    )
    .unwrap();
    let proof = join::sign_join_proof(challenge.clone(), &device_key).unwrap();
    let redemption =
        join::verify_join_proof(&proof, &challenge, &device, &RevocationList::new(), ns).unwrap();
    let mut peer = StatePeer::new(
        ROOM,
        governor.clone(),
        vec![governor.clone(), host.clone()],
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let channel = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
    chat.put(&channel, "kind", "voice").unwrap();
    chat.put(&channel, "workspace", "home").unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(ROOM, &governor, &[governor.clone(), host.clone()]).unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                record_id: "".into(),
                kind: "chat".into(),
                bytes: chat.save(),
            }],
        },
        &governor_key,
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    assert!(
        peer.admit_invited_member(&redemption, &governor_key, now)
            .unwrap()
    );
    let policy = media::RoomMediaV1 {
        version: 1,
        room: *uuid::Uuid::parse_str(ROOM).unwrap().as_bytes(),
        authorization_revision: 1,
        authorization_basis: media::security_basis(
            peer.admitted_room().unwrap(),
            &peer.revocations(),
        )
        .unwrap(),
        calls: BTreeMap::from([(
            [68; 32],
            media::CallV1 {
                binding: peer.resolve_media_binding(APP, "", "voice").unwrap(),
                epoch: 1,
                phase: media::CallPhaseV1::Active,
                allowed: media::DirectionsV1 {
                    publish: true,
                    subscribe: true,
                },
                denied: BTreeMap::new(),
            },
        )]),
    };
    peer.author_media_policy(&policy, &governor_key, now)
        .unwrap();
    let messages = peer
        .durable_bootstrap_messages(&governor_key, now, 1)
        .unwrap();
    assert!(peer.validated_media_policy().unwrap().is_some());
    let bootstrap = join::JoinBootstrap {
        version: 1,
        invite_id: verified.invite_id(),
        device_cert: device.clone(),
        role: join::MemberRole::Editor,
        room: ROOM.into(),
        workspace_id,
        authority: governor.clone(),
        certified_roster: peer.certificates(),
        revocations: peer.revocations(),
        records: peer.rows(),
        governance_messages: vec![postcard::to_stdvec(&messages[0]).unwrap()],
        generation: 1,
    };
    let intent = crate::network::invite::VerifiedNativeEnrollmentIntent::from_test_proof(
        verified,
        join::JoinCompleteRequest { proof },
        device,
        hint,
        now,
    )
    .unwrap();
    (intent, bootstrap, messages, governor.device_public_key, now)
}
fn fixture(owner: &NativeWorkspaceOwner) -> Fixture {
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = owner.prepare_enrollment(&intent, now).unwrap();
    let enrollment = VerifiedNativeEnrollment::finalize_test(intent, pending, bootstrap).unwrap();
    Fixture {
        enrollment,
        messages,
        governor,
        now,
    }
}

fn tree_at(path: &std::path::Path) -> WorkspaceTreeHandle {
    let mut tree = WorkspaceTree::bootstrap_default();
    let id = tree.active;
    assert!(tree.set_room(id, RoomId::parse(ROOM).unwrap()));
    tree.persist_path = Some(path.into());
    tree.shared()
}

#[test]
fn native_owner_commits_exact_chat_and_reopens() {
    // Losing the admitted Chat bytes or treating a room-only policy as ready
    // must fail this actual disk/reopen test.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone())
        .expect("native owner must attach to the actual state-file writer");
    let fixture = fixture(&owner);
    let room = RoomId::parse(ROOM).unwrap();
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    owner
        .with_current(room, |view| {
            assert!(
                view.peer().validated_media_policy().is_err(),
                "room policy alone is not the exact Chat prerequisite"
            );
            Ok(())
        })
        .unwrap();
    for message in fixture.messages.into_iter().skip(1) {
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(fixture.governor),
                message,
                fixture.now,
            )
            .unwrap();
    }
    owner
        .with_current(room, |view| {
            let policy = view
                .peer()
                .validated_media_policy()?
                .ok_or("missing admitted policy")?;
            assert_eq!(policy.calls.len(), 1);
            assert_eq!(policy.calls[&[68; 32]].binding.record_id, "");
            assert!(view.peer().document(APP, "").is_some());
            Ok(())
        })
        .unwrap();
    drop(owner);
    drop(tree);
    let mut restored = persist::load_tree_from(&path).unwrap();
    restored.persist_path = Some(path.clone());
    let reopened = NativeWorkspaceOwner::open(restored.shared()).unwrap();
    reopened
        .with_current(room, |view| {
            assert!(view.peer().admission_v5_selected());
            assert_eq!(
                view.peer()
                    .validated_media_policy()?
                    .ok_or("policy lost on restart")?
                    .calls
                    .len(),
                1
            );
            assert!(view.peer().document(APP, "").is_some());
            Ok(())
        })
        .unwrap();
    let disk: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        disk["schema_version"], 3,
        "old shell must refuse before dropping durable authority"
    );
}

#[test]
fn native_pending_complete_survives_restart_and_cancellation_preserves_exact_proof() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let id = owner.prepare_enrollment(&intent, now).unwrap();
    let durable = owner.pending_enrollment(id.room()).unwrap().unwrap();
    assert_eq!(durable.intent_bytes(), intent.durable_bytes().unwrap());
    drop(owner);
    drop(tree);
    let mut loaded = persist::load_tree_from(&path).unwrap();
    loaded.persist_path = Some(path);
    let owner = NativeWorkspaceOwner::open(loaded.shared()).unwrap();
    let retried = owner
        .prepare_enrollment(&intent, now + std::time::Duration::from_secs(120))
        .unwrap();
    assert_eq!(
        retried, id,
        "expired challenge cannot create a different retry proof"
    );
    assert!(owner.cancel_enrollment(id.room(), &id).unwrap());
    let cancelled = owner.pending_enrollment(id.room()).unwrap().unwrap();
    assert_eq!(cancelled.intent_bytes(), durable.intent_bytes());
    assert_eq!(cancelled.status(), PendingEnrollmentStatus::Cancelled);
    let late = VerifiedNativeEnrollment::finalize_test(intent, id.clone(), bootstrap).unwrap();
    assert!(owner.install(&late, now).is_err());
    assert!(owner.with_current(id.room(), |_| Ok(())).is_err());
    assert!(
        owner.contains_room(id.room()),
        "cancelled unresolved room must never fall back to legacy"
    );
}

#[test]
fn native_pending_not_published_failure_returns_no_send_authority() {
    use atomic_file::test_support::{Checkpoint, with_failure};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = NativeWorkspaceOwner::open(tree).unwrap();
    let before = std::fs::read(&path).unwrap();
    let (intent, _, _, _, now) = raw_fixture();
    assert!(
        with_failure(Checkpoint::BeforePublish, || owner
            .prepare_enrollment(&intent, now))
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(owner.pending_enrollment(intent.room()).unwrap().is_none());
    assert!(!owner.contains_room(intent.room()));
    owner.prepare_enrollment(&intent, now).unwrap();
}

#[test]
fn native_admission_not_published_keeps_previous_committed_chat_and_disk() {
    use atomic_file::test_support::{Checkpoint, with_failure};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = NativeWorkspaceOwner::open(tree).unwrap();
    let fixture = fixture(&owner);
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    let before = std::fs::read(&path).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    assert!(
        with_failure(Checkpoint::BeforePublish, || owner.receive(
            room,
            TransportOrigin::RetainedRelayDelta(fixture.governor),
            fixture.messages[1].clone(),
            fixture.now
        ))
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    owner
        .with_current(room, |view| {
            assert!(view.peer().validated_media_policy().is_err());
            Ok(())
        })
        .unwrap();
    for message in fixture.messages.into_iter().skip(1) {
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(fixture.governor),
                message,
                fixture.now,
            )
            .unwrap();
    }
    owner
        .with_current(room, |view| {
            assert!(view.peer().validated_media_policy()?.is_some());
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_uncertain_publication_fences_owner_ui_and_live_listener_until_reopen() {
    use atomic_file::test_support::{Checkpoint, with_failure};
    use std::sync::atomic::{AtomicUsize, Ordering};
    for checkpoint in [Checkpoint::AfterPublish, Checkpoint::BeforeCommit] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let tree = tree_at(&path);
        let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
        let fixture = fixture(&owner);
        owner.install(&fixture.enrollment, fixture.now).unwrap();
        let room = RoomId::parse(ROOM).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let _subscription = owner.observe_fences(Arc::new(move |got, _| {
            assert_eq!(got, room);
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(
            with_failure(checkpoint, || owner.receive(
                room,
                TransportOrigin::RetainedRelayDelta(fixture.governor),
                fixture.messages[1].clone(),
                fixture.now
            ))
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(owner.with_current(room, |_| Ok(())).is_err());
        let published = std::fs::read(&path).unwrap();
        assert!(persist::save_tree_to(&tree.read(), &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), published);
        drop(owner);
        drop(tree);
        let mut restored = persist::load_tree_from(&path).unwrap();
        restored.persist_path = Some(path);
        let recovered = NativeWorkspaceOwner::open(restored.shared()).unwrap();
        recovered
            .with_current(room, |view| {
                assert!(view.peer().document(APP, "").is_some());
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn native_stale_ui_snapshot_cannot_erase_admitted_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let fixture = fixture(&owner);
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    let stale = tree.read().clone();
    for message in fixture.messages.into_iter().skip(1) {
        owner
            .receive(
                RoomId::parse(ROOM).unwrap(),
                TransportOrigin::RetainedRelayDelta(fixture.governor),
                message,
                fixture.now,
            )
            .unwrap();
    }
    let committed = std::fs::read(&path).unwrap();
    assert!(persist::save_tree_to(&stale, &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    persist::save_tree_to(&tree.read(), &path).unwrap();
}

#[test]
fn native_v1_hidden_records_and_future_files_are_preserved_and_refused() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let _fixture = fixture(&owner);
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    drop(owner);
    drop(tree);
    assert_eq!(
        raw["schema_version"].as_u64(),
        Some(u64::from(persist::NATIVE_STATE_SCHEMA_VERSION))
    );
    let supported = std::fs::read(&path).unwrap();
    assert!(persist::load_tree_from(&path).is_ok());
    assert_eq!(std::fs::read(&path).unwrap(), supported);
    for version in [1, persist::NATIVE_STATE_SCHEMA_VERSION + 1] {
        raw["schema_version"] = version.into();
        let bytes = serde_json::to_vec(&raw).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(persist::load_tree_from(&path).is_err());
        let (_, notice) = persist::load_or_default(&path);
        assert!(notice.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    assert_eq!(
        qos_desktop::STATE_SCHEMA_VERSION,
        1,
        "portable schema remains unchanged"
    );
}

fn bootstrap_candidate(
    owner: &NativeWorkspaceOwner,
    generation: u64,
    now: SystemTime,
) -> join::TransportHint {
    use tilo_relay::delivery_v3::descriptor;
    owner
        .with_current(RoomId::parse(ROOM).unwrap(), |view| {
            let authority = descriptor::DescriptorAuthority {
                peer: view.peer(),
                pinned_governor: view.governor(),
                pinned_host: view.host().unwrap(),
                workspace: view.workspace(),
                generation: descriptor::GenerationExpectation::FirstAccepted {
                    minimum_generation: 1,
                },
                allowed_delivery_capabilities: 3,
                now,
            };
            let ns = enrollment::ns(now)?;
            descriptor::issue_transport_hint(
                &descriptor::DescriptorSettings {
                    origin: "https://native.example".into(),
                    delivery_capabilities: 3,
                    issued_at_ns: ns,
                    expires_at_ns: ns + 120_000_000_000,
                    generation,
                },
                &authority,
                &SigningKey::from_bytes(&[69; 32]),
            )
            .map_err(|e| e.to_string())
        })
        .unwrap()
}
fn bootstrap_response(
    pending: &NativeBootstrap,
    hint: &join::TransportHint,
    now: SystemTime,
) -> tilo_relay::delivery_v3::media_wire::BootstrapResponse {
    use ed25519_dalek::Signer;
    use tilo_relay::delivery_v3::{descriptor, media_wire};
    let metadata = descriptor::inspect_persisted_hint(hint, &hint.host).unwrap();
    let mut response = media_wire::BootstrapResponse {
        version: 1,
        request: pending.request().clone(),
        host_certificate: postcard::to_stdvec(&hint.host).unwrap(),
        hint: postcard::to_stdvec(hint).unwrap(),
        generation: metadata.generation,
        descriptor_digest: metadata.descriptor_digest,
        issued_at_ns: enrollment::ns(now).unwrap(),
        expires_at_ns: enrollment::ns(now).unwrap() + 10_000_000_000,
        signature: [[0; 32]; 2],
    };
    response.signature = media_wire::split_signature(
        SigningKey::from_bytes(&[61; 32])
            .sign(&response.signing_digest().unwrap())
            .to_bytes(),
    );
    response
}
#[test]
fn native_bootstrap_floor_requires_exact_governor_ack_and_survives_stale_save_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let fixture = fixture(&owner);
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    let stale = tree.read().clone();
    let hint = bootstrap_candidate(&owner, 2, fixture.now);
    let key = SigningKey::from_bytes(&[63; 32]);
    let pending = owner
        .begin_bootstrap(room, &hint, &key, fixture.now)
        .unwrap();
    let mut bad = bootstrap_response(&pending, &hint, fixture.now);
    bad.request.nonce[0] ^= 1;
    assert!(owner.finish_bootstrap(pending, &bad, fixture.now).is_err());
    assert!(matches!(
        owner
            .with_current(room, |v| v.descriptor_expectation())
            .unwrap(),
        GenerationExpectation::FirstAccepted { .. }
    ));
    let pending = owner
        .begin_bootstrap(room, &hint, &key, fixture.now)
        .unwrap();
    let response = bootstrap_response(&pending, &hint, fixture.now);
    owner
        .finish_bootstrap(pending, &response, fixture.now)
        .unwrap();
    assert!(persist::save_tree_to(&stale, &path).is_err());
    let continuity = owner.with_current(room, |v| Ok(v.continuity())).unwrap();
    let same = owner
        .begin_bootstrap(room, &hint, &key, fixture.now)
        .unwrap();
    let response = bootstrap_response(&same, &hint, fixture.now);
    owner
        .finish_bootstrap(same, &response, fixture.now)
        .unwrap();
    assert_eq!(
        owner.with_current(room, |v| Ok(v.continuity())).unwrap(),
        continuity
    );
    let rollback = bootstrap_candidate(&owner, 1, fixture.now);
    assert!(
        owner
            .begin_bootstrap(room, &rollback, &key, fixture.now)
            .is_err()
    );
    let equivocation =
        bootstrap_candidate(&owner, 2, fixture.now + std::time::Duration::from_nanos(1));
    assert!(
        owner
            .begin_bootstrap(room, &equivocation, &key, fixture.now)
            .is_err()
    );
    drop(stale);
    drop(owner);
    drop(tree);
    let mut loaded = persist::load_tree_from(&path).unwrap();
    loaded.persist_path = Some(path);
    let reopened = NativeWorkspaceOwner::open(loaded.shared()).unwrap();
    match reopened
        .with_current(room, |v| v.descriptor_expectation())
        .unwrap()
    {
        GenerationExpectation::Current {
            generation,
            descriptor_digest,
        } => {
            assert_eq!(generation, 2);
            assert_eq!(descriptor_digest, response.descriptor_digest);
        }
        _ => panic!("restart erased acknowledged descriptor floor"),
    }
    assert!(
        reopened
            .begin_bootstrap(room, &rollback, &key, fixture.now)
            .is_err()
    );
}
#[test]
fn native_identity_replacement_synchronously_fences_and_rejects_pending_bootstrap() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let owner = NativeWorkspaceOwner::open(tree_at(&path)).unwrap();
    let fixture = fixture(&owner);
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    let hint = bootstrap_candidate(&owner, 1, fixture.now);
    let pending = owner
        .begin_bootstrap(room, &hint, &SigningKey::from_bytes(&[63; 32]), fixture.now)
        .unwrap();
    let response = bootstrap_response(&pending, &hint, fixture.now);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let _subscription = owner.observe_fences(Arc::new(move |got, _| {
        assert_eq!(got, room);
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    owner.invalidate_identity(room);
    owner.invalidate_identity(room);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(owner.with_current(room, |_| Ok(())).is_err());
    assert!(
        owner
            .finish_bootstrap(pending, &response, fixture.now)
            .is_err()
    );
    assert!(owner.contains_room(room));
}

#[test]
fn native_file_writer_excludes_competing_owner_and_encoded_overflow_preserves_disk() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let fixture = fixture(&owner);
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    let mut independently_loaded = persist::load_tree_from(&path).unwrap();
    independently_loaded.persist_path = Some(path.clone());
    assert!(NativeWorkspaceOwner::open(independently_loaded.shared()).is_err());
    let committed = std::fs::read(&path).unwrap();
    let mut oversized = tree.read().clone();
    oversized.workspaces[0].name = "x".repeat(store::MAX_STATE_BYTES);
    assert!(persist::save_tree_to(&oversized, &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    drop(oversized);
    let mut oversized_authority = tree.read().clone();
    // Malformed oversized bytes must be refused before any generic tree clone or publication.
    oversized_authority
        .native_authority
        .rooms
        .get_mut(&RoomId::parse(ROOM).unwrap())
        .unwrap()
        .peer = vec![0; store::MAX_AUTHORITY_BYTES / 2];
    assert!(persist::save_tree_to(&oversized_authority, &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    persist::save_tree_to(&tree.read(), &path).unwrap();
}

#[test]
fn native_local_document_commit_receipt_and_exact_retry_use_actual_selected_device() {
    use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let owner = NativeWorkspaceOwner::open(tree_at(&path)).unwrap();
    let fixture = fixture(&owner);
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    for message in fixture.messages.into_iter().skip(1) {
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(fixture.governor),
                message,
                fixture.now,
            )
            .unwrap();
    }
    let message = owner
        .with_current(room, |view| {
            let mut chat = AutoCommit::load(&view.peer().document(APP, "").unwrap()).unwrap();
            let before = chat.get_heads();
            chat.put(ROOT, "topic", "A real native local edit").unwrap();
            chat.commit();
            let changes = chat
                .get_changes(&before)
                .into_iter()
                .flat_map(|change| change.raw_bytes().to_vec())
                .collect();
            view.peer().clone().publish_incremental(
                APP,
                "",
                changes,
                &SigningKey::from_bytes(&[63; 32]),
                fixture.now,
            )
        })
        .unwrap();
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("actual signed app mutation expected")
    };
    let request = OwnerCommitRequest {
        room: ROOM.into(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: "".into(),
            envelope: envelope.clone(),
        },
    };
    let receipt = owner.commit_document(request.clone(), fixture.now).unwrap();
    assert_eq!(receipt.binding, request.binding().unwrap());
    let committed = std::fs::read(&path).unwrap();
    let retry = owner
        .commit_document(request, fixture.now + std::time::Duration::from_secs(3600))
        .unwrap();
    assert_eq!(retry.generation, receipt.generation);
    assert_eq!(retry.aggregate_hash, receipt.aggregate_hash);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        committed,
        "exact retry must not rewrite or manufacture a new admission"
    );
    let mut foreign = *envelope;
    foreign.author = collab_types::Ed25519PublicKey::from_bytes(fixture.governor);
    collab_types::sign_op(&mut foreign, &SigningKey::from_bytes(&[61; 32])).unwrap();
    assert!(
        owner
            .commit_document(
                OwnerCommitRequest {
                    room: ROOM.into(),
                    operation: CommitOperation::Document {
                        app: APP.into(),
                        record: "".into(),
                        envelope: Box::new(foreign)
                    }
                },
                fixture.now
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), committed);
}

pub(crate) fn test_enrolled_owner() -> (
    tempfile::TempDir,
    NativeWorkspaceOwner,
    RoomId,
    SystemTime,
    SigningKey,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let owner = NativeWorkspaceOwner::open(tree_at(&path)).unwrap();
    let fixture = fixture(&owner);
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    for message in fixture.messages.into_iter().skip(1) {
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(fixture.governor),
                message,
                fixture.now,
            )
            .unwrap();
    }
    (
        directory,
        owner,
        room,
        fixture.now,
        SigningKey::from_bytes(&[63; 32]),
    )
}
pub(crate) fn test_descriptor(
    owner: &NativeWorkspaceOwner,
    generation: u64,
    now: SystemTime,
) -> join::TransportHint {
    bootstrap_candidate(owner, generation, now)
}
pub(crate) fn test_bootstrap_response(
    pending: &NativeBootstrap,
    hint: &join::TransportHint,
    now: SystemTime,
) -> tilo_relay::delivery_v3::media_wire::BootstrapResponse {
    bootstrap_response(pending, hint, now)
}
fn fresh_slot(
    path: &std::path::Path,
) -> (
    WorkspaceTreeHandle,
    crate::shell::state::WorkspaceId,
    RoomId,
) {
    let mut native = WorkspaceTree::bootstrap_default();
    let local = native.active;
    let previous = native.room_of(local).unwrap();
    assert_ne!(previous, RoomId::parse(ROOM).unwrap());
    native.persist_path = Some(path.into());
    (native.shared(), local, previous)
}
#[test]
fn native_enrollment_target_slot_publishes_new_room_only_with_actual_owner_barrier() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let (tree, local, previous) = fresh_slot(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let pending = owner
        .prepare_enrollment_into(&intent, local, now)
        .expect("native enrollment must durably bind its actual selected local slot");
    assert_eq!(
        tree.read().room_of(local),
        Some(previous),
        "preparation must not join or expose the new room"
    );
    let enrolled = VerifiedNativeEnrollment::finalize_test(intent, pending, bootstrap).unwrap();
    owner.install(&enrolled, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    assert_eq!(tree.read().room_of(local), Some(room));
    owner
        .with_current(room, |view| {
            assert!(view.peer().has_governance());
            Ok(())
        })
        .unwrap();
    let disk = persist::load_tree_from(&path).unwrap();
    assert_eq!(disk.room_of(local), Some(room));
    assert!(disk.requires_native_owner(room));
}
#[test]
fn native_enrollment_target_slot_reassignment_rejects_late_complete_and_keeps_proof() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let (tree, local, _) = fresh_slot(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, local, now).unwrap();
    let enrolled =
        VerifiedNativeEnrollment::finalize_test(intent, pending.clone(), bootstrap).unwrap();
    let replaced = RoomId::new();
    assert!(tree.write().set_room(local, replaced));
    assert!(owner.install(&enrolled, now).is_err());
    assert_eq!(tree.read().room_of(local), Some(replaced));
    assert_eq!(
        owner
            .pending_enrollment(pending.room())
            .unwrap()
            .unwrap()
            .id(),
        &pending
    );
    assert!(owner.with_current(pending.room(), |_| Ok(())).is_err());
}
#[test]
fn native_enrollment_target_slot_failed_barrier_preserves_previous_binding() {
    use atomic_file::test_support::{Checkpoint, with_failure};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let (tree, local, previous) = fresh_slot(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, local, now).unwrap();
    let enrolled =
        VerifiedNativeEnrollment::finalize_test(intent, pending.clone(), bootstrap).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(with_failure(Checkpoint::BeforePublish, || owner.install(&enrolled, now)).is_err());
    assert_eq!(tree.read().room_of(local), Some(previous));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        owner
            .pending_enrollment(pending.room())
            .unwrap()
            .unwrap()
            .id(),
        &pending
    );
    owner.install(&enrolled, now).unwrap();
    assert_eq!(tree.read().room_of(local), Some(pending.room()));
}

#[test]
fn native_authenticated_bootstrap_admits_historical_signed_coverage_but_ongoing_input_does_not() {
    let directory = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let historical = now - std::time::Duration::from_secs(3600);
    let (intent, mut bootstrap, messages, governor, _) = raw_fixture_at(historical);
    let owner =
        NativeWorkspaceOwner::open(tree_at(&directory.path().join("complete.json"))).unwrap();
    let pending = owner.prepare_enrollment(&intent, historical).unwrap();
    bootstrap.governance_messages = messages
        .iter()
        .map(|message| postcard::to_stdvec(message).unwrap())
        .collect();
    let complete = VerifiedNativeEnrollment::finalize_test(intent, pending, bootstrap).unwrap();
    owner.install(&complete, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    owner
        .with_current(room, |view| {
            assert!(view.peer().validated_media_policy()?.is_some());
            Ok(())
        })
        .unwrap();

    let (intent, bootstrap, _, _, _) = raw_fixture_at(historical);
    let separate =
        NativeWorkspaceOwner::open(tree_at(&directory.path().join("ongoing.json"))).unwrap();
    let pending = separate.prepare_enrollment(&intent, historical).unwrap();
    let complete = VerifiedNativeEnrollment::finalize_test(intent, pending, bootstrap).unwrap();
    separate.install(&complete, now).unwrap();
    assert!(
        separate
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(governor),
                messages[1].clone(),
                now
            )
            .is_err(),
        "an ongoing unproved historical input must not inherit authenticated Complete's bootstrap privilege"
    );
}

#[test]
fn native_uncertain_new_pending_proof_remains_required_without_send_receipt() {
    use atomic_file::test_support::{Checkpoint, with_failure};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let (tree, local, _) = fresh_slot(&path);
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    let (intent, _, _, _, now) = raw_fixture();
    let room = intent.room();
    assert!(!tree.read().requires_native_owner(room));
    assert!(
        with_failure(Checkpoint::AfterPublish, || owner
            .prepare_enrollment_into(&intent, local, now))
        .is_err()
    );
    assert!(
        owner.contains_room(room),
        "possibly published pending intent cannot revert to legacy"
    );
    assert!(tree.read().requires_native_owner(room));
    assert!(owner.with_current(room, |_| Ok(())).is_err());
    assert!(
        owner.pending_enrollment(room).is_err(),
        "fenced writer cannot hand out retry-send custody"
    );
}

#[test]
fn native_reopen_refuses_each_partial_acknowledged_descriptor_pair() {
    for removed in ["floor", "hint"] {
        let (directory, owner, room, now, key) = test_enrolled_owner();
        let hint = test_descriptor(&owner, 3, now);
        let pending = owner.begin_bootstrap(room, &hint, &key, now).unwrap();
        let response = test_bootstrap_response(&pending, &hint, now);
        owner.finish_bootstrap(pending, &response, now).unwrap();
        let path = directory.path().join("state.json");
        drop(owner);
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["native_authority"]["rooms"][ROOM][removed] = serde_json::Value::Null;
        let bytes = serde_json::to_vec(&raw).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut loaded = persist::load_tree_from(&path).unwrap();
        loaded.persist_path = Some(path.clone());
        let tree = loaded.shared();
        assert!(
            NativeWorkspaceOwner::open(tree.clone()).is_err(),
            "partial {removed} cannot reset an acknowledged floor or skip descriptor validation"
        );
        assert!(tree.read().requires_native_owner(room));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
#[serial_test::serial(process_env)]
fn native_slot_barrier_and_stale_flush_preserve_actual_acl_and_publisher() {
    use crate::shell::state::{CapOverride, Effect, Scope, Subject, Verb};
    use atomic_file::test_support::{Checkpoint, with_failure_on_nth};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            let _ = qos_desktop::fs_authority::wait_for_fs_authority_publication_idle(
                Duration::from_secs(2),
            );
            // SAFETY: this test owns the existing process_env serial group.
            unsafe {
                for (name, old) in self.0.drain(..) {
                    match old {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let acl = directory.path().join("acl.json");
    let observed_acl = directory.path().join("published-acl.json");
    let observed_state = directory.path().join("published-state.json");
    let publisher = directory.path().join("publisher.sh");
    std::fs::write(&publisher, "#!/bin/sh\ncp \"$5\" \"$QOS_NATIVE_OWNER_ACL_OBSERVED\"\ncp \"$QOS_NATIVE_OWNER_STATE\" \"$QOS_NATIVE_OWNER_STATE_OBSERVED\"\n").unwrap();
    std::fs::set_permissions(&publisher, std::fs::Permissions::from_mode(0o700)).unwrap();
    let values = [
        ("QOS_FS_AUTHORITY_PUBLISHER", publisher.clone()),
        ("QOS_FS_AUTHORITY_CACHE_DIR", directory.path().join("cache")),
        ("QOS_NATIVE_OWNER_ACL_OBSERVED", observed_acl.clone()),
        ("QOS_NATIVE_OWNER_STATE", path.clone()),
        ("QOS_NATIVE_OWNER_STATE_OBSERVED", observed_state.clone()),
        ("TILO_IDENTITY_ROOT", directory.path().join("identity")),
        ("XDG_CONFIG_HOME", directory.path().join("config")),
    ];
    let _restore = RestoreEnv(
        values
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect(),
    );
    // SAFETY: this test owns the existing process_env serial group.
    unsafe {
        for (name, value) in &values {
            std::env::set_var(name, value);
        }
    }
    let (tree, local, previous) = fresh_slot(&path);
    {
        let mut native = tree.write();
        native.fs_acl_path = Some(acl.clone());
        let mut old = CapOverride::new(
            Subject::User(collab_types::Ed25519PublicKey::from_bytes([201; 32])),
            Scope::Workspace,
            Effect::Grant,
            Verb::Read,
        );
        old.id = "old-room-only".into();
        native.workspaces[0].capabilities.push(old);
    }
    let owner = NativeWorkspaceOwner::open(tree.clone()).unwrap();
    tree.read().export_fs_capabilities();
    assert!(
        qos_desktop::fs_authority::wait_for_fs_authority_publication_idle(Duration::from_secs(2))
    );
    let baseline_acl = std::fs::read(&acl).unwrap();
    assert!(String::from_utf8_lossy(&baseline_acl).contains("old-room-only"));
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let id = owner.prepare_enrollment_into(&intent, local, now).unwrap();
    let complete = VerifiedNativeEnrollment::finalize_test(intent, id, bootstrap).unwrap();
    let stale = tree.read().clone();
    let before = std::fs::read(&path).unwrap();
    std::fs::remove_file(&observed_acl).unwrap();
    std::fs::remove_file(&observed_state).unwrap();
    // The first actual publication must be state.json, never a candidate ACL export.
    let failed = with_failure_on_nth(Checkpoint::BeforePublish, 1, || {
        owner.install(&complete, now)
    });
    assert!(
        failed.is_err(),
        "candidate side-effect consumed the failure before the actual owner barrier"
    );
    assert!(
        qos_desktop::fs_authority::wait_for_fs_authority_publication_idle(Duration::from_secs(2))
    );
    assert_eq!(tree.read().room_of(local), Some(previous));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&acl).unwrap(), baseline_acl);
    assert!(
        !observed_acl.exists(),
        "no authority publisher may see the failed candidate"
    );
    owner.install(&complete, now).unwrap();
    assert!(
        qos_desktop::fs_authority::wait_for_fs_authority_publication_idle(Duration::from_secs(2))
    );
    let committed_acl = std::fs::read(&acl).unwrap();
    assert!(!String::from_utf8_lossy(&committed_acl).contains("old-room-only"));
    assert_eq!(std::fs::read(&observed_acl).unwrap(), committed_acl);
    let published_state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&observed_state).unwrap()).unwrap();
    assert!(
        published_state["native_authority"]["rooms"][ROOM].is_object(),
        "actual publisher must observe acknowledged final owner state"
    );
    std::fs::remove_file(&observed_acl).unwrap();
    std::fs::remove_file(&observed_state).unwrap();
    stale.flush_to_disk();
    assert!(
        qos_desktop::fs_authority::wait_for_fs_authority_publication_idle(Duration::from_secs(2))
    );
    assert_eq!(std::fs::read(&acl).unwrap(), committed_acl);
    assert!(
        !observed_acl.exists(),
        "stale save refusal must also refuse its authority export"
    );
}
