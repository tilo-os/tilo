//! These tests use actual signed membership and native document admission.
use super::*;
use crate::shell::{persist, state::WorkspaceTree};
use automerge::{AutoCommit, ROOT, transaction::Transactable};
use ed25519_dalek::SigningKey;
use std::{sync::Arc, time::SystemTime};
use tilo_delivery_protocol::descriptor::GenerationExpectation;
use tilo_joined_room::testing::{APP, ROOM, raw_fixture, raw_fixture_at};
use tilo_joined_room::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};
use tilo_peer_state::{NeighbourMsg, TransportOrigin, join};

struct Fixture {
    enrollment: VerifiedNativeEnrollment,
    messages: Vec<NeighbourMsg>,
    governor: [u8; 32],
    now: SystemTime,
}
/// The state tree the owner publishes into.
pub(crate) fn tree_of(owner: &NativeWorkspaceOwner) -> WorkspaceTreeHandle {
    let store: Arc<dyn std::any::Any + Send + Sync> = tilo_joined_room::testing::store(owner);
    store
        .downcast::<TreeStore>()
        .expect("a desktop owner publishes into its state tree")
        .tree
        .clone()
}
/// Enroll into the slot that already shows the invitation's room.
pub(super) fn prepare_enrollment(
    owner: &NativeWorkspaceOwner,
    intent: &VerifiedNativeEnrollmentIntent,
    now: SystemTime,
) -> Result<PendingEnrollmentId, String> {
    let local = tree_of(owner)
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|slot| slot.room == intent.room())
        .map(|slot| slot.id)
        .ok_or("enrollment requires an explicit existing local slot")?;
    owner.prepare_enrollment_into(intent, Some(local), now)
}
fn fixture(owner: &NativeWorkspaceOwner) -> Fixture {
    let (intent, bootstrap, messages, governor, now) = raw_fixture();
    let pending = prepare_enrollment(owner, &intent, now).unwrap();
    let enrollment = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
    Fixture {
        enrollment,
        messages,
        governor,
        now,
    }
}

fn tree_at(path: &std::path::Path) -> WorkspaceTreeHandle {
    let mut tree = WorkspaceTree::bootstrap_default();
    let id = tree.desktop.active;
    assert!(tree.set_room(id, RoomId::parse(ROOM).unwrap()));
    tree.persist_path = Some(path.into());
    tree.shared()
}

/// Owner publications and state-file encodes this thread made while `run` ran.
fn publications_and_encodes<T>(run: impl FnOnce() -> T) -> (T, usize, usize) {
    let publications = PUBLICATIONS_ON_THIS_THREAD.with(std::cell::Cell::get);
    let encodes = persist::ENCODES_ON_THIS_THREAD.with(std::cell::Cell::get);
    let value = run();
    (
        value,
        PUBLICATIONS_ON_THIS_THREAD
            .with(std::cell::Cell::get)
            .saturating_sub(publications),
        persist::ENCODES_ON_THIS_THREAD
            .with(std::cell::Cell::get)
            .saturating_sub(encodes),
    )
}

#[test]
fn native_owner_publication_encodes_the_state_file_once_and_keeps_the_live_document() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = open(tree.clone()).unwrap();
    let heads_before = tree.write().current_heads();
    assert!(
        !heads_before.is_empty(),
        "the fixture's document has history"
    );
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let (pending, publications, encodes) =
        publications_and_encodes(|| prepare_enrollment(&owner, &intent, now));
    let pending = pending.unwrap();
    assert!(publications > 0, "a pending enrollment is published");
    assert_eq!(
        encodes, publications,
        "each publication encodes the state file once"
    );
    let enrollment = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
    let (installed, publications, encodes) =
        publications_and_encodes(|| owner.install(&enrollment, now));
    installed.unwrap();
    assert!(publications > 0, "an install is published");
    assert_eq!(
        encodes, publications,
        "each publication encodes the state file once"
    );
    let tree = tree.read();
    assert!(
        heads_before
            .iter()
            .all(|head| { automerge::ReadDoc::get_change_by_hash(&tree.doc, head).is_some() }),
        "the installed tree carries the live document's history, not a fresh one"
    );
}

#[test]
fn native_owner_commits_exact_chat_and_reopens() {
    // Losing the admitted Chat bytes or treating a room-only policy as ready
    // must fail this actual disk/reopen test.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner =
        open(tree.clone()).expect("native owner must attach to the actual state-file writer");
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
    let reopened = open(restored.shared()).unwrap();
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
    let owner = open(tree.clone()).unwrap();
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let id = prepare_enrollment(&owner, &intent, now).unwrap();
    let durable = owner.pending_enrollment(id.room()).unwrap().unwrap();
    assert_eq!(durable.intent_bytes(), intent.durable_bytes().unwrap());
    drop(owner);
    drop(tree);
    let mut loaded = persist::load_tree_from(&path).unwrap();
    loaded.persist_path = Some(path);
    let owner = open(loaded.shared()).unwrap();
    let retried =
        prepare_enrollment(&owner, &intent, now + std::time::Duration::from_secs(120)).unwrap();
    assert_eq!(
        retried, id,
        "expired challenge cannot create a different retry proof"
    );
    assert!(owner.cancel_enrollment(id.room(), &id).unwrap());
    let cancelled = owner.pending_enrollment(id.room()).unwrap().unwrap();
    assert_eq!(cancelled.intent_bytes(), durable.intent_bytes());
    assert_eq!(cancelled.status(), PendingEnrollmentStatus::Cancelled);
    let late = tilo_joined_room::testing::finalize(intent, id.clone(), bootstrap).unwrap();
    assert!(owner.install(&late, now).is_err());
    assert!(owner.with_current(id.room(), |_| Ok(())).is_err());
    assert!(
        owner.contains_room(id.room()),
        "cancelled unresolved room must never fall back to legacy"
    );
}

#[test]
fn native_pending_not_published_failure_returns_no_send_authority() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = open(tree).unwrap();
    let before = std::fs::read(&path).unwrap();
    let (intent, _, _, _, now) = raw_fixture();
    assert!(
        with_failure(Checkpoint::BeforePublish, || prepare_enrollment(
            &owner, &intent, now
        ))
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(owner.pending_enrollment(intent.room()).unwrap().is_none());
    assert!(!owner.contains_room(intent.room()));
    prepare_enrollment(&owner, &intent, now).unwrap();
}

#[test]
fn native_admission_not_published_keeps_previous_committed_chat_and_disk() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let owner = open(tree).unwrap();
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    for checkpoint in [Checkpoint::AfterPublish, Checkpoint::BeforeCommit] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let tree = tree_at(&path);
        let owner = open(tree.clone()).unwrap();
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
        let recovered = open(restored.shared()).unwrap();
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
    let owner = open(tree.clone()).unwrap();
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
    let owner = open(tree.clone()).unwrap();
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
        tilo_desktop_model::STATE_SCHEMA_VERSION,
        1,
        "the desktop schema is versioned apart from the native file envelope"
    );
}

fn bootstrap_candidate(
    owner: &NativeWorkspaceOwner,
    generation: u64,
    now: SystemTime,
) -> join::TransportHint {
    use tilo_delivery_protocol::descriptor;
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
            let ns = tilo_joined_room::testing::ns(now)?;
            descriptor::issue_transport_hint(
                &descriptor::DescriptorSettings {
                    origin: "https://native.example".into(),
                    delivery_capabilities: 3,
                    issued_at_ns: ns,
                    expires_at_ns: ns.checked_add(120_000_000_000).unwrap(),
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
) -> tilo_delivery_protocol::bootstrap::BootstrapResponse {
    use ed25519_dalek::Signer;
    use tilo_delivery_protocol::{bootstrap, descriptor};
    let metadata = descriptor::inspect_persisted_hint(hint, &hint.host).unwrap();
    let mut response = bootstrap::BootstrapResponse {
        version: 1,
        request: pending.request().clone(),
        host_certificate: postcard::to_stdvec(&hint.host).unwrap(),
        hint: postcard::to_stdvec(hint).unwrap(),
        generation: metadata.generation,
        descriptor_digest: metadata.descriptor_digest,
        issued_at_ns: tilo_joined_room::testing::ns(now).unwrap(),
        expires_at_ns: tilo_joined_room::testing::ns(now)
            .unwrap()
            .checked_add(10_000_000_000)
            .unwrap(),
        signature: [[0; 32]; 2],
    };
    response.signature = bootstrap::split_signature(
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
    let owner = open(tree.clone()).unwrap();
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
            .with_current(room, super::NativeCommittedView::descriptor_expectation)
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
    let reopened = open(loaded.shared()).unwrap();
    match reopened
        .with_current(room, super::NativeCommittedView::descriptor_expectation)
        .unwrap()
    {
        GenerationExpectation::Current {
            generation,
            descriptor_digest,
        } => {
            assert_eq!(generation, 2);
            assert_eq!(descriptor_digest, response.descriptor_digest);
        }
        GenerationExpectation::FirstAccepted { .. } => {
            panic!("restart erased acknowledged descriptor floor")
        }
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
    let owner = open(tree_at(&path)).unwrap();
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
    let owner = open(tree.clone()).unwrap();
    let fixture = fixture(&owner);
    owner.install(&fixture.enrollment, fixture.now).unwrap();
    let mut independently_loaded = persist::load_tree_from(&path).unwrap();
    independently_loaded.persist_path = Some(path.clone());
    assert!(open(independently_loaded.shared()).is_err());
    let committed = std::fs::read(&path).unwrap();
    let mut oversized = tree.read().clone();
    oversized.desktop.workspaces[0].name = "x".repeat(store::MAX_STATE_BYTES);
    assert!(persist::save_tree_to(&oversized, &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    drop(oversized);
    let mut oversized_authority = tree.read().clone();
    // Malformed oversized bytes must be refused before any generic tree clone or publication.
    // A cap's worth of peer bytes encodes as a third more base64 text than the cap.
    tilo_joined_room::testing::set_stored_peer(
        &mut oversized_authority.native_authority,
        RoomId::parse(ROOM).unwrap(),
        vec![0; store::MAX_AUTHORITY_BYTES],
    );
    assert!(persist::save_tree_to(&oversized_authority, &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    persist::save_tree_to(&tree.read(), &path).unwrap();
}

#[test]
fn native_local_document_commit_receipt_and_exact_retry_use_actual_selected_device() {
    use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let owner = open(tree_at(&path)).unwrap();
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
            record: String::new(),
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
                        record: String::new(),
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
    let owner = open(tree_at(&path)).unwrap();
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
) -> tilo_delivery_protocol::bootstrap::BootstrapResponse {
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
    let local = native.desktop.active;
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
    let owner = open(tree.clone()).unwrap();
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let pending = owner
        .prepare_enrollment_into(&intent, Some(local), now)
        .expect("native enrollment must durably bind its actual selected local slot");
    assert_eq!(
        tree.read().room_of(local),
        Some(previous),
        "preparation must not join or expose the new room"
    );
    let enrolled = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
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
    let owner = open(tree.clone()).unwrap();
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let pending = owner
        .prepare_enrollment_into(&intent, Some(local), now)
        .unwrap();
    let enrolled = tilo_joined_room::testing::finalize(intent, pending.clone(), bootstrap).unwrap();
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
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let (tree, local, previous) = fresh_slot(&path);
    let owner = open(tree.clone()).unwrap();
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let pending = owner
        .prepare_enrollment_into(&intent, Some(local), now)
        .unwrap();
    let enrolled = tilo_joined_room::testing::finalize(intent, pending.clone(), bootstrap).unwrap();
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
    let owner = open(tree_at(&directory.path().join("complete.json"))).unwrap();
    let pending = prepare_enrollment(&owner, &intent, historical).unwrap();
    bootstrap.governance_messages = messages
        .iter()
        .map(|message| postcard::to_stdvec(message).unwrap())
        .collect();
    let complete = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&complete, now).unwrap();
    let room = RoomId::parse(ROOM).unwrap();
    owner
        .with_current(room, |view| {
            assert!(view.peer().validated_media_policy()?.is_some());
            Ok(())
        })
        .unwrap();

    let (intent, bootstrap, _, _, _) = raw_fixture_at(historical);
    let separate = open(tree_at(&directory.path().join("ongoing.json"))).unwrap();
    let pending = prepare_enrollment(&separate, &intent, historical).unwrap();
    let complete = tilo_joined_room::testing::finalize(intent, pending, bootstrap).unwrap();
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
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let (tree, local, _) = fresh_slot(&path);
    let owner = open(tree.clone()).unwrap();
    let (intent, _, _, _, now) = raw_fixture();
    let room = intent.room();
    assert!(!tree.read().requires_native_owner(room));
    assert!(
        with_failure(Checkpoint::AfterPublish, || owner.prepare_enrollment_into(
            &intent,
            Some(local),
            now
        ))
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
            open(tree.clone()).is_err(),
            "partial {removed} cannot reset an acknowledged floor or skip descriptor validation"
        );
        assert!(tree.read().requires_native_owner(room));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
#[serial_test::serial(process_env)]
fn native_slot_barrier_and_stale_flush_preserve_actual_acl_and_publisher() {
    use crate::shell::state::{Access, CapOverride, Effect, Scope, Subject};
    use std::os::unix::fs::PermissionsExt;
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure_on_nth};
    /// Lets background fs-authority publication settle before the wrapped
    /// scope restores the environment it reads.
    struct RestoreEnv {
        _scope: tilo_test_env::ScopedEnv,
    }
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            let _ = crate::shell::fs_authority::wait_for_fs_authority_publication_idle(
                tilo_test_env::hang_bound(),
            );
        }
    }
    #[track_caller]
    fn settle_publication(step: &str) {
        crate::shell::fs_authority::wait_for_fs_authority_publication_idle(
            tilo_test_env::hang_bound(),
        )
        .unwrap_or_else(|report| panic!("{step}: {report}"));
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    let acl = directory.path().join("acl.json");
    let observed_acl = directory.path().join("published-acl.json");
    let observed_state = directory.path().join("published-state.json");
    let publisher = directory.path().join("publisher.sh");
    std::fs::write(&publisher, "#!/bin/sh\ncp \"$3\" \"$TILO_NATIVE_OWNER_ACL_OBSERVED\"\ncp \"$TILO_NATIVE_OWNER_STATE\" \"$TILO_NATIVE_OWNER_STATE_OBSERVED\"\n").unwrap();
    std::fs::set_permissions(&publisher, std::fs::Permissions::from_mode(0o700)).unwrap();
    let values = [
        ("TILO_FS_AUTHORITY_PUBLISHER", publisher),
        ("TILO_NATIVE_OWNER_ACL_OBSERVED", observed_acl.clone()),
        ("TILO_NATIVE_OWNER_STATE", path.clone()),
        ("TILO_NATIVE_OWNER_STATE_OBSERVED", observed_state.clone()),
        ("TILO_IDENTITY_ROOT", directory.path().join("identity")),
        ("XDG_CONFIG_HOME", directory.path().join("config")),
    ];
    let mut env = tilo_test_env::ScopedEnv::new();
    for (name, value) in &values {
        env.set(name, value);
    }
    let _restore = RestoreEnv { _scope: env };
    let (tree, local, previous) = fresh_slot(&path);
    {
        let mut native = tree.write();
        native.fs_acl_path = Some(acl.clone());
        let mut old = CapOverride::new(
            Subject::User(collab_types::Ed25519PublicKey::from_bytes([201; 32])),
            Scope::Workspace,
            Effect::Grant,
            Access::Read,
        );
        old.id = "old-room-only".into();
        native.desktop.workspaces[0].sandbox.push(old);
    }
    let owner = open(tree.clone()).unwrap();
    tree.read().export_fs_capabilities();
    settle_publication("baseline ACL export must publish");
    let baseline_acl = std::fs::read(&acl).unwrap();
    assert!(String::from_utf8_lossy(&baseline_acl).contains("old-room-only"));
    let (intent, bootstrap, _, _, now) = raw_fixture();
    let id = owner
        .prepare_enrollment_into(&intent, Some(local), now)
        .unwrap();
    let complete = tilo_joined_room::testing::finalize(intent, id, bootstrap).unwrap();
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
    settle_publication("failed install must leave publication idle");
    assert_eq!(tree.read().room_of(local), Some(previous));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&acl).unwrap(), baseline_acl);
    assert!(
        !observed_acl.exists(),
        "no authority publisher may see the failed candidate"
    );
    owner.install(&complete, now).unwrap();
    settle_publication("committed install must publish its ACL");
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
    stale.queue_save();
    settle_publication("stale flush must leave publication idle");
    assert_eq!(std::fs::read(&acl).unwrap(), committed_acl);
    assert!(
        !observed_acl.exists(),
        "stale save refusal must also refuse its authority export"
    );
}
