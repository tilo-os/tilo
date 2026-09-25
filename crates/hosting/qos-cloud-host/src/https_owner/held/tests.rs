use super::*;
use automerge::resource_limits::NativeAllowance;
use qos_peer_state::{PeerLimits, PeerRegistry};
use qos_relay::delivery_v3::commit::ConsumerState;
use qos_session_peer::ingress::HostBytes;

fn limits() -> DecodeLimits {
    DecodeLimits {
        maximum_bytes: 32 * 1024 * 1024,
        allocation_bytes: 16 * 1024 * 1024,
        collection_entries: 65536,
    }
}
struct Seal(NativeAllowance);
impl Drop for Seal {
    fn drop(&mut self) {
        self.0.finish_turn();
    }
}
fn work<T>(pool: &HostBytes, action: impl FnOnce(&mut PeerWork<'_>) -> T) -> T {
    work_with_credit(pool, |work, _| action(work))
}
fn work_with_credit<T>(
    pool: &HostBytes,
    action: impl FnOnce(&mut PeerWork<'_>, &NativeAllowance) -> T,
) -> T {
    let initial = pool.native_allowance().unwrap();
    let initial_seal = Seal(initial.clone());
    let mut registry = PeerRegistry::new(initial).unwrap();
    drop(initial_seal);
    let current = pool.native_allowance().unwrap();
    let _seal = Seal(current.clone());
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    action(&mut work, &current)
}
fn fixture() -> (
    tempfile::TempDir,
    CloudStoreSource,
    PolicyTime,
    ConsumerState,
) {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    // Inert metadata candidate for the actual storage/resource component. This
    // is not consumer registration authority or a fabricated VerifiedSession.
    let consumer = ConsumerState {
        id: [41; 16],
        device,
        root: store.state.bound.as_ref().unwrap().root,
        authorization_view: [42; 32],
        acknowledged: None,
    };
    let source = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("current Source required")
    };
    (dir, source, time, consumer)
}
fn opened(value: Current<Result<HeldStoreTransaction>>) -> HeldStoreTransaction {
    match value {
        Current::Current(Ok(value)) => value,
        Current::Current(Err(error)) => panic!("held Store: {error}"),
        Current::Busy => panic!("unexpected busy"),
        Current::Denied => panic!("unexpected denied"),
    }
}

#[tokio::test]
async fn cloud_held_store_actual_paid_candidate_barrier_and_noop_reopen() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let available = pool.available_bytes();
    let (dir, source, time, consumer) = fixture();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let generation = work(&pool, |work| {
        let mut held = opened(HeldStoreTransaction::open(&source, &time, work, limits()));
        let generation = held.state().generation;
        assert!(
            held.commit_metadata(MetadataChange::Consumer(consumer.clone()), work, limits())
                .unwrap()
        );
        assert_eq!(held.state().generation, generation + 1);
        assert!(
            !held
                .commit_metadata(MetadataChange::Consumer(consumer.clone()), work, limits())
                .unwrap()
        );
        assert_eq!(held.state().generation, generation + 1);
        generation + 1
    });
    assert_eq!(
        pool.available_bytes(),
        available,
        "original paid backings released"
    );
    assert_ne!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    let store = crate::delivery::access::try_open(dir.path(), "agent-admission").unwrap();
    assert_eq!(store.state.generation, generation);
    let ledger = store.state.delivery.as_ref().unwrap().ledger();
    assert_eq!(ledger.consumers.get(&consumer.device), Some(&consumer));
    assert_eq!(ledger.resolved_through, generation);
    assert!(
        !ledger.replay.contains_key(&generation),
        "metadata cannot invent replay"
    );
}

#[tokio::test]
async fn cloud_held_store_capacity_and_busy_preserve_original_state() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let (dir, source, time, _) = fixture();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    let locked = crate::delivery::access::try_open(dir.path(), "agent-admission").unwrap();
    work(&pool, |work| {
        assert!(matches!(
            HeldStoreTransaction::open(&source, &time, work, limits()),
            Current::Busy
        ))
    });
    drop(locked);
    work(&pool, |work| {
        let mut tiny = limits();
        tiny.maximum_bytes = 1;
        assert!(matches!(
            HeldStoreTransaction::open(&source, &time, work, tiny),
            Current::Current(Err(CloudDeliveryError::Capacity))
        ));
    });
    assert!(time.now_ns().is_ok(), "pressure must not retire Source");
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn cloud_held_store_actual_uncertain_barrier_never_publishes_twice() {
    use atomic_file::test_support::Checkpoint;
    for point in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
        let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
        let pool = transport.host_bytes().unwrap();
        let (dir, source, time, consumer) = fixture();
        work(&pool, |work| {
            let mut held = opened(HeldStoreTransaction::open(&source, &time, work, limits()));
            let result = crate::storage::test_support::with_save_failure(dir.path(), point, || {
                held.commit_metadata(MetadataChange::Consumer(consumer.clone()), work, limits())
            });
            assert!(matches!(result, Err(CloudDeliveryError::OwnerUncertain(_))));
            assert!(matches!(held.publication_error.as_ref(),
                Some(crate::storage::transaction::PublishFailure::Io { failure, error })
                if Some(*failure) == held.failed.get()
                    && atomic_file::publication_state(error).is_some()));
            let observed = std::fs::read(dir.path().join("state.json")).unwrap();
            assert!(
                held.commit_metadata(MetadataChange::Consumer(consumer.clone()), work, limits())
                    .is_err()
            );
            assert_eq!(
                std::fs::read(dir.path().join("state.json")).unwrap(),
                observed
            );
        });
        assert!(
            time.now_ns().is_err(),
            "uncertainty retires original origin"
        );
        crate::delivery::access::try_open(dir.path(), "agent-admission").unwrap();
    }
}

#[tokio::test]
async fn cloud_held_store_rejects_foreign_origin_and_replaced_directory() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let (dir, source, time, _) = fixture();
    let foreign = source.0.clock.new_origin();
    let wrong = foreign.capture().unwrap();
    work(&pool, |work| {
        assert!(matches!(
            HeldStoreTransaction::open(&source, &wrong, work, limits()),
            Current::Denied
        ))
    });
    assert!(time.now_ns().is_err());
    let (dir2, source2, time2, _) = fixture();
    let original = std::fs::read(dir2.path().join("state.json")).unwrap();
    let moved = dir2.path().with_extension("held-original");
    std::fs::rename(dir2.path(), &moved).unwrap();
    std::fs::rename(dir.path(), dir2.path()).unwrap();
    std::fs::write(dir2.path().join("state.json"), &original).unwrap();
    let denied = work(&pool, |work| {
        matches!(
            HeldStoreTransaction::open(&source2, &time2, work, limits()),
            Current::Denied
        )
    });
    let unchanged = std::fs::read(moved.join("state.json")).unwrap() == original
        && std::fs::read(dir2.path().join("state.json")).unwrap() == original;
    std::fs::rename(dir2.path(), dir.path()).unwrap();
    std::fs::rename(&moved, dir2.path()).unwrap();
    assert!(denied && unchanged);
}

#[tokio::test]
async fn cloud_held_store_real_heap_is_precharged_through_native_and_barrier() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let (_dir, source, time, consumer) = fixture();
    let initial = pool.native_allowance().unwrap();
    let initial_seal = Seal(initial.clone());
    let mut registry = PeerRegistry::new(initial).unwrap();
    drop(initial_seal);
    let current = pool.native_allowance().unwrap();
    let _seal = Seal(current.clone());
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    crate::delivery::resources::precharged_test(&current, || {
        let mut held = opened(HeldStoreTransaction::open(
            &source,
            &time,
            &mut work,
            limits(),
        ));
        assert!(
            held.commit_metadata(MetadataChange::Consumer(consumer), &mut work, limits())
                .unwrap()
        );
        drop(held);
    });
}

#[tokio::test]
async fn cloud_held_store_native_and_historical_enrollment_corruption_is_terminal() {
    use qos_user_identity::{
        RevocationEntry, RevocationReason, sign_device_cert, sign_revocation_entry,
    };
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    for case in 0..4 {
        let (dir, source, time, _) = fixture();
        let path = dir.path().join("state.json");
        let mut state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut cert: DeviceCert =
            serde_json::from_value(state["bound"]["authority"].clone()).unwrap();
        match case {
            0 => {
                state["bound"]["peer"] = serde_json::json!([0]);
            }
            1 | 2 => {
                cert.device_public_key = ed25519_dalek::SigningKey::from_bytes(&[61; 32])
                    .verifying_key()
                    .to_bytes();
                cert.issued_at_ns = 1;
                cert.expires_at_ns = Some(2);
                let root =
                    ed25519_dalek::SigningKey::from_bytes(&[if case == 1 { 7 } else { 60 }; 32]);
                cert.user_public_key = root.verifying_key().to_bytes();
                sign_device_cert(&mut cert, &root).unwrap();
                if case == 1 {
                    cert.signature[0] ^= 1;
                }
                state["bound"]["roster"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::to_value(cert).unwrap());
            }
            3 => {
                let mut revoked = RevocationEntry {
                    user_public_key: cert.user_public_key,
                    revoked_device_public_key: cert.device_public_key,
                    revoked_at_ns: 3,
                    reason: RevocationReason::Retired,
                    signature: [0; 64],
                };
                sign_revocation_entry(
                    &mut revoked,
                    &ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
                )
                .unwrap();
                state["bound"]["revocations"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::to_value(revoked).unwrap());
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&state).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        work(&pool, |work| {
            assert!(
                matches!(
                    HeldStoreTransaction::open(&source, &time, work, limits()),
                    Current::Denied
                ),
                "case {case}"
            )
        });
        assert!(time.now_ns().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn cloud_held_store_retired_origin_and_foreign_work_cannot_commit() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    for foreign in [false, true] {
        let (dir, source, time, consumer) = fixture();
        let original = std::fs::read(dir.path().join("state.json")).unwrap();
        work(&pool, |current| {
            let mut held = opened(HeldStoreTransaction::open(
                &source,
                &time,
                current,
                limits(),
            ));
            let result = if foreign {
                work(&pool, |other| {
                    held.commit_metadata(MetadataChange::Consumer(consumer), other, limits())
                })
            } else {
                source.retire();
                held.commit_metadata(MetadataChange::Consumer(consumer), current, limits())
            };
            assert_eq!(result, Err(CloudDeliveryError::Denied));
        });
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            original
        );
        assert!(time.now_ns().is_err());
    }
}

#[tokio::test]
async fn cloud_held_store_commit_keeps_original_directory_after_ambient_replacement() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let (dir, source, time, consumer) = fixture();
    let (replacement, replacement_source, replacement_time, _) = fixture();
    let original = std::fs::read(dir.path().join("state.json")).unwrap();
    let foreign = std::fs::read(replacement.path().join("state.json")).unwrap();
    let moved = dir.path().with_extension("held-original");
    let result = work(&pool, |work| {
        let mut held = opened(HeldStoreTransaction::open(&source, &time, work, limits()));
        std::fs::rename(dir.path(), &moved).unwrap();
        std::fs::rename(replacement.path(), dir.path()).unwrap();
        let result = held.commit_metadata(MetadataChange::Consumer(consumer), work, limits());
        let retained = std::fs::read(moved.join("state.json")).unwrap();
        let ambient = std::fs::read(dir.path().join("state.json")).unwrap();
        drop(held);
        let refused = matches!(
            HeldStoreTransaction::open(&source, &time, work, limits()),
            Current::Denied
        );
        // Restore the fixture paths before asserting, even if publication failed.
        std::fs::rename(dir.path(), replacement.path()).unwrap();
        std::fs::rename(&moved, dir.path()).unwrap();
        (result, retained, ambient, refused)
    });
    assert_eq!(result.0, Ok(true));
    assert_ne!(result.1, original);
    assert_eq!(result.2, foreign);
    assert!(result.3);
    assert!(time.now_ns().is_err());
    assert!(replacement_time.now_ns().is_ok());
    work(&pool, |work| {
        drop(opened(HeldStoreTransaction::open(
            &replacement_source,
            &replacement_time,
            work,
            limits(),
        )));
    });
}

fn private_fixture() -> (tempfile::TempDir, CloudStoreSource, PolicyTime) {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    let (caller, request) = crate::agent_owner_tests::prepare(&store, device, "held-private");
    store.commit_agent_request(&request, &caller).unwrap();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let source = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("current Source required")
    };
    (dir, source, time)
}
fn limited_native_work<T>(pool: &HostBytes, action: impl FnOnce(&mut PeerWork<'_>) -> T) -> T {
    let initial = pool.native_allowance().unwrap();
    let seal = Seal(initial.clone());
    let mut registry = PeerRegistry::new(initial).unwrap();
    drop(seal);
    let current = pool.native_allowance().unwrap();
    let _seal = Seal(current.clone());
    let mut turn = registry
        .begin_turn(
            current,
            PeerLimits {
                normalized_bytes: 1,
                ..PeerLimits::default()
            },
        )
        .unwrap();
    action(&mut turn.work().unwrap())
}

#[tokio::test]
async fn cloud_held_store_private_history_success_and_retryable_native_pressure() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let (dir, source, time) = private_fixture();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    limited_native_work(&pool, |work| {
        assert!(matches!(
            HeldStoreTransaction::open(&source, &time, work, limits()),
            Current::Current(Err(CloudDeliveryError::Capacity))
        ));
    });
    assert!(
        time.now_ns().is_ok(),
        "temporary work pressure must not retire Source"
    );
    work(&pool, |work| {
        let held = opened(HeldStoreTransaction::open(&source, &time, work, limits()));
        let Some(qos_peer_state::owner_commit::PrivateAgentState::V1(ledger)) =
            &held.state().private_agent_ledger
        else {
            panic!("original private selection required")
        };
        assert_eq!(ledger.records.len(), 1);
        assert_eq!(ledger.mutations.len(), 1);
    });
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn cloud_held_store_private_invalid_history_precedes_native_restore_and_retires() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let (dir, source, time) = private_fixture();
    let path = dir.path().join("state.json");
    let mut state: crate::State = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let Some(qos_peer_state::owner_commit::PrivateAgentState::V1(ledger)) =
        &mut state.private_agent_ledger
    else {
        unreachable!()
    };
    // Structurally valid DTO with an impossible retained commit generation.
    // The native bytes stay genuine. With the tiny continuing native counter,
    // missing private validation would reach restore and return retryable Capacity.
    ledger.mutations.values_mut().next().unwrap().generation = 0;
    let before = serde_json::to_vec(&state).unwrap();
    std::fs::write(&path, &before).unwrap();
    limited_native_work(&pool, |work| {
        assert!(matches!(
            HeldStoreTransaction::open(&source, &time, work, limits()),
            Current::Denied
        ));
    });
    assert!(time.now_ns().is_err());
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OpenPhase {
    AfterPrivate,
    AfterNative,
    AfterDelivery,
}
type PhaseHook = (OpenPhase, Box<dyn FnOnce()>);
thread_local! {
    static PHASE_HOOK: std::cell::RefCell<Option<PhaseHook>> = const { std::cell::RefCell::new(None) };
}
pub(super) fn at_phase(phase: OpenPhase) {
    let action = PHASE_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|(expected, _)| *expected == phase)
        {
            slot.take().map(|(_, action)| action)
        } else {
            None
        }
    });
    if let Some(action) = action {
        action();
    }
}
fn with_phase<T>(
    phase: OpenPhase,
    action: impl FnOnce() + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<PhaseHook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PHASE_HOOK.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let old = PHASE_HOOK.with(|slot| slot.replace(Some((phase, Box::new(action)))));
    let _restore = Restore(old);
    operation()
}

fn signed_historical_expiring_store() -> (
    tempfile::TempDir,
    CloudStoreSource,
    PolicyTime,
    qos_action_broker::peer_actions::PolicyClockTestControl,
) {
    use ed25519_dalek::SigningKey;
    use qos_action_broker::peer_actions::OwnerPolicyClock;
    // Current enrollment deliberately forbids expiring host certificates. Build
    // a cryptographically genuine historical Store fixture, not an alternate
    // enrollment path: real generated Store key, root signatures, native seed,
    // signed publications, original Store barriers, and explicit selections.
    let dir = tempfile::tempdir().unwrap();
    let mut store = crate::Store::open(dir.path(), "held-expiry").unwrap();
    let initial = qos_relay::clock::now_ns();
    let root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let request = crate::tests::approved(store.inspect(initial).unwrap(), &root, &device);
    let mut authority = request.host_certificate;
    authority.expires_at_ns = Some(initial + 3_600_000_000_000);
    qos_user_identity::sign_device_cert(&mut authority, &root).unwrap();
    let key = store.key();
    let mut peer = qos_peer_state::StatePeer::new(
        &store.state.room,
        authority.clone(),
        vec![authority.clone()],
        qos_user_identity::RevocationList::new(),
        crate::now_time(initial),
    )
    .unwrap();
    peer.install_seed(
        crate::initial_state(&store.state.room, &authority, &key).unwrap(),
        &key,
        crate::now_time(initial),
    )
    .unwrap();
    peer.admit_certificate(
        request.browser_certificate.clone(),
        &key,
        crate::now_time(initial),
    )
    .unwrap();
    let messages = crate::durable_bootstrap_messages(
        &mut peer,
        &key,
        crate::now_time(initial),
        store.next_generation().unwrap(),
    )
    .unwrap();
    let workspace = qos_relay::room_workspace_id(&store.state.room).unwrap();
    let mut directory =
        crate::WorkspaceDirectory::new(authority.user_public_key, authority.device_public_key);
    directory
        .add_workspace(workspace, "Historical signed fixture")
        .unwrap();
    directory
        .set_member_devices(
            workspace,
            &[
                authority.device_public_key,
                request.browser_certificate.device_public_key,
            ],
        )
        .unwrap();
    directory
        .set_host_devices(workspace, &[authority.device_public_key])
        .unwrap();
    let mut state = store.state.clone();
    state.challenge = None;
    state.bound = Some(crate::Bound {
        root: authority.user_public_key,
        authority: authority.clone(),
        roster: vec![authority, request.browser_certificate],
        revocations: vec![],
        peer: peer.durable().unwrap(),
        directory: directory.save(),
    });
    state.outbox = crate::baseline_publications(messages).unwrap();
    store.commit(state).unwrap();
    let caller = store
        .agent_caller(device.verifying_key().to_bytes())
        .unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    let (caller, request) = crate::agent_owner_tests::prepare(
        &store,
        device.verifying_key().to_bytes(),
        "expiry-private",
    );
    store.commit_agent_request(&request, &caller).unwrap();
    store.activate_delivery_storage(initial).unwrap();
    drop(store);
    // The original existing-only loader accepts the exact signed durable owner.
    let store = crate::delivery::access::try_open(dir.path(), "held-expiry").unwrap();
    let (clock, control) = OwnerPolicyClock::for_test(initial);
    let source = CloudStoreSource::bind_with_clock(&store, clock).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("genuine current source required")
    };
    (dir, source, time, control)
}

async fn assert_expiry_at(phase: OpenPhase) {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let available = pool.available_bytes();
    let (dir, source, time, control) = signed_historical_expiring_store();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    // First establish that this exact owner/state/time is accepted while current.
    work(&pool, |work| {
        let held = opened(HeldStoreTransaction::open(&source, &time, work, limits()));
        assert_eq!(
            held.state().bound.as_ref().unwrap().authority,
            source.0.authority
        );
    });
    assert_eq!(pool.available_bytes(), available);
    let reached = std::rc::Rc::new(Cell::new(false));
    let mark = reached.clone();
    with_phase(
        phase,
        move || {
            mark.set(true);
            control.advance_monotonic(std::time::Duration::from_secs(3600));
        },
        || {
            work(&pool, |work| {
                assert!(
                    matches!(
                        HeldStoreTransaction::open(&source, &time, work, limits()),
                        Current::Denied
                    ),
                    "expired pinned owner cannot escape the held constructor"
                );
            });
        },
    );
    assert!(reached.get(), "the exact phase boundary must be exercised");
    assert!(
        time.now_ns().is_err(),
        "denial must retire the original origin"
    );
    assert!(
        source.0.clock.now_ns().is_ok(),
        "the configured shared clock stays live"
    );
    assert_eq!(pool.available_bytes(), available);
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn cloud_held_store_expiry_after_private_validation_is_terminal() {
    assert_expiry_at(OpenPhase::AfterPrivate).await;
}
#[tokio::test]
async fn cloud_held_store_expiry_after_native_restore_is_terminal() {
    assert_expiry_at(OpenPhase::AfterNative).await;
}

// Actual signed native document admission and Store commit, followed by binding
// the original Source while the persisted bytes are still valid.
fn delivery_fixture() -> (tempfile::TempDir, CloudStoreSource, PolicyTime, Vec<u8>) {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    let v4 = store.state.bound.as_ref().unwrap().peer.clone();
    let now = qos_relay::clock::now_ns();
    store.activate_delivery_storage(now).unwrap();
    let (before, mut peer) = store.peer_transition_at(now).unwrap();
    let document = peer.document("so.tilo.chat", "").unwrap();
    peer.publish_document(
        "so.tilo.chat",
        "",
        document,
        &store.key(),
        crate::now_time(now),
    )
    .unwrap();
    let mut next = store.state.clone();
    next.outbox.extend(
        crate::durable_mutation_publications(
            &mut peer,
            &before,
            &store.key(),
            crate::now_time(now),
            store.next_generation().unwrap(),
        )
        .unwrap(),
    );
    next.bound.as_mut().unwrap().peer = peer.durable().unwrap();
    store.commit(next).unwrap();
    assert!(
        !store
            .state
            .delivery
            .as_ref()
            .unwrap()
            .ledger()
            .replay
            .last_key_value()
            .unwrap()
            .1
            .admitted_operations
            .is_empty()
    );
    let source = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("current Source required")
    };
    (dir, source, time, v4)
}

#[tokio::test]
async fn cloud_held_store_delivery_valid_exact_admissions_is_unchanged() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let available = pool.available_bytes();
    let (dir, source, time, _) = delivery_fixture();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    work(&pool, |work| {
        let held = opened(HeldStoreTransaction::open(&source, &time, work, limits()));
        let entry = held
            .state()
            .delivery
            .as_ref()
            .unwrap()
            .ledger()
            .replay
            .last_key_value()
            .unwrap()
            .1;
        assert!(!entry.admitted_operations.is_empty());
        let read = held.peer.read(work).unwrap();
        assert!(read.admission_v5_selected());
        assert!(
            entry
                .admitted_operations
                .iter()
                .all(|id| read.contains_admitted_operation(id))
        );
        assert_eq!(serde_json::to_vec(held.state()).unwrap(), before);
    });
    assert!(time.now_ns().is_ok());
    assert_eq!(pool.available_bytes(), available);
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn cloud_held_store_delivery_persisted_semantic_corruption_is_terminal() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let available = pool.available_bytes();
    for case in 0..5 {
        let (dir, source, time, v4) = delivery_fixture();
        let path = dir.path().join("state.json");
        let mut wire: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let want = match case {
            0 => {
                wire["delivery"]["policy_revision"] = serde_json::json!("0".repeat(64));
                "delivery policy revision mismatch"
            }
            1 | 2 => {
                let bytes: Vec<u8> =
                    serde_json::from_value(wire["delivery"]["ledger"].clone()).unwrap();
                let mut ledger =
                    qos_relay::delivery_v3::commit::DeliveryLedgerV1::decode(&bytes).unwrap();
                if case == 1 {
                    ledger.replay.last_entry().unwrap().get_mut().digest[0] ^= 1;
                } else {
                    ledger
                        .replay
                        .last_entry()
                        .unwrap()
                        .get_mut()
                        .admitted_operations[0] = [231; 32];
                    // Recompute the exact final replay digest through the real
                    // wire/State boundary; no private DTO constructor or hook.
                    wire["delivery"]["ledger"] =
                        serde_json::to_value(ledger.encode().unwrap()).unwrap();
                    let state: crate::State = serde_json::from_value(wire.clone()).unwrap();
                    let delivery = state.delivery.as_ref().unwrap();
                    let last = delivery.ledger().replay.last_key_value().unwrap().1;
                    let digest = crate::delivery::replay_digest::exact(
                        &state,
                        delivery.store_generation(),
                        last,
                    )
                    .unwrap();
                    ledger.replay.last_entry().unwrap().get_mut().digest = digest;
                }
                wire["delivery"]["ledger"] =
                    serde_json::to_value(ledger.encode().unwrap()).unwrap();
                "invalid Cloud replay evidence"
            }
            3 => {
                wire["bound"]["peer"] = serde_json::to_value(v4).unwrap();
                "native admission v5 required"
            }
            4 => {
                wire["generation"] = serde_json::json!(wire["generation"].as_u64().unwrap() + 1);
                "delivery barrier mismatch"
            }
            _ => unreachable!(),
        };
        // Actual State deserialization and serialization run the original
        // immutable ledger/pool structural codecs. Native bytes remain genuine.
        let state: crate::State = serde_json::from_value(wire).unwrap();
        assert_eq!(
            crate::delivery::validate(&state, time.now_ns().unwrap())
                .unwrap_err()
                .to_string(),
            want
        );
        let bytes = serde_json::to_vec(&state).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let reached = std::rc::Rc::new(Cell::new(false));
        let mark = reached.clone();
        with_phase(
            OpenPhase::AfterNative,
            move || mark.set(true),
            || {
                work(&pool, |work| {
                    assert!(
                        matches!(
                            HeldStoreTransaction::open(&source, &time, work, limits()),
                            Current::Denied
                        ),
                        "persisted semantic case {case} must not yield a holder"
                    );
                });
            },
        );
        assert!(reached.get(), "genuine bounded restore must have succeeded");
        assert!(time.now_ns().is_err(), "denial retires the original Source");
        assert!(source.0.clock.now_ns().is_ok());
        assert_eq!(pool.available_bytes(), available);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn cloud_held_store_delivery_original_work_pressure_is_retryable() {
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let available = pool.available_bytes();
    let (dir, source, time, _) = delivery_fixture();
    let before = std::fs::read(dir.path().join("state.json")).unwrap();
    work_with_credit(&pool, |work, current| {
        let all = std::rc::Rc::new(std::cell::RefCell::new(None));
        let keep = all.clone();
        let original = current.clone();
        let reached = std::rc::Rc::new(Cell::new(false));
        let mark = reached.clone();
        with_phase(
            OpenPhase::AfterNative,
            move || {
                mark.set(true);
                // Reserve from the exact ledger underlying this live PeerWork,
                // after native restoration. Hold it until the constructor returns.
                *keep.borrow_mut() = Some(original.reserve(original.available_bytes()).unwrap());
            },
            || {
                assert!(matches!(
                    HeldStoreTransaction::open(&source, &time, work, limits()),
                    Current::Current(Err(CloudDeliveryError::Capacity))
                ));
            },
        );
        assert!(reached.get());
        drop(all.borrow_mut().take());
    });
    assert!(
        time.now_ns().is_ok(),
        "delivery pressure must not retire Source"
    );
    assert_eq!(pool.available_bytes(), available);
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
    // A fresh actual turn can retry this same Source, time, and original file.
    work(&pool, |work| {
        drop(opened(HeldStoreTransaction::open(
            &source,
            &time,
            work,
            limits(),
        )));
    });
    assert!(time.now_ns().is_ok());
    assert_eq!(pool.available_bytes(), available);
    assert_eq!(
        std::fs::read(dir.path().join("state.json")).unwrap(),
        before
    );
}

#[tokio::test]
async fn cloud_held_store_expiry_after_delivery_validation_is_terminal() {
    assert_expiry_at(OpenPhase::AfterDelivery).await;
}
