use crate::{State, Store, delivery, now_time};
use qos_peer_state::transport::Retention;
use qos_relay::delivery_v3::commit::{CommitError, PublicationRef};

fn fixture() -> (tempfile::TempDir, Store, Vec<u8>, u64) {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    let v4 = store.state.bound.as_ref().unwrap().peer.clone();
    let now = qos_relay::clock::now_ns();
    store.activate_delivery_storage(now).unwrap();
    let (before, mut peer) = store.peer_transition_at(now).unwrap();
    let document = peer.document("so.tilo.chat", "").unwrap();
    peer.publish_document("so.tilo.chat", "", document, &store.key(), now_time(now))
        .unwrap();
    let mut next = store.state.clone();
    next.outbox.extend(
        crate::durable_mutation_publications(
            &mut peer,
            &before,
            &store.key(),
            now_time(now),
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
            .ledger
            .replay
            .last_key_value()
            .unwrap()
            .1
            .admitted_operations
            .is_empty(),
        "must exercise real native admission membership"
    );
    (dir, store, v4, qos_relay::clock::now_ns())
}
fn error(state: &State, now: u64) -> anyhow::Error {
    delivery::validate(state, now).expect_err("invalid retained state must be refused")
}
fn latest_mut(state: &mut State) -> &mut qos_relay::delivery_v3::commit::ReplayCommit {
    state
        .delivery
        .as_mut()
        .unwrap()
        .ledger
        .replay
        .last_entry()
        .unwrap()
        .into_mut()
}
fn rehash_latest(state: &mut State) {
    let d = state.delivery.as_ref().unwrap();
    let seq = *d.ledger.replay.last_key_value().unwrap().0;
    let digest =
        delivery::replay_digest::exact(state, d.store_generation(), &d.ledger.replay[&seq])
            .unwrap();
    latest_mut(state).digest = digest;
}
#[test]
fn cloud_delivery_validation_real_admissions_require_digest_and_index_membership() {
    let (dir, store, _, now) = fixture();
    delivery::validate(&store.state, now).unwrap();
    let peer = delivery::committed_peer(&store.state, now).unwrap();
    let empty = crate::StatePeer::new(
        &store.state.room,
        peer.authority().clone(),
        vec![peer.authority().clone()],
        qos_user_identity::RevocationList::new(),
        now_time(now),
    )
    .unwrap();
    let actual = peer.admissions_since(&empty).unwrap();
    let last = store
        .state
        .delivery
        .as_ref()
        .unwrap()
        .ledger
        .replay
        .last_key_value()
        .unwrap()
        .1;
    assert!(
        last.admitted_operations
            .iter()
            .all(|id| actual.iter().any(|a| a.operation_id() == *id))
    );
    let mut unknown = store.state.clone();
    let id = [231; 32];
    assert!(!actual.iter().any(|a| a.operation_id() == id));
    latest_mut(&mut unknown).admitted_operations[0] = id;
    rehash_latest(&mut unknown);
    unknown.delivery.as_ref().unwrap().validate().unwrap();
    assert_eq!(
        error(&unknown, now).to_string(),
        "invalid Cloud replay evidence"
    );
    let mut digest = store.state.clone();
    latest_mut(&mut digest).digest[0] ^= 1;
    digest.delivery.as_ref().unwrap().validate().unwrap();
    assert_eq!(
        error(&digest, now).to_string(),
        "invalid Cloud replay evidence"
    );
    // Actual Store open also refuses the semantically corrupt but structurally
    // encodable state, and must preserve the original persisted bytes.
    let machine = store.state.machine_id.clone();
    drop(store);
    let bytes = serde_json::to_vec(&unknown).unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, &bytes).unwrap();
    assert!(Store::open(dir.path(), &machine).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}
#[test]
fn cloud_delivery_validation_preserves_outer_structure_and_barrier_error_order() {
    let (_, store, _, now) = fixture();
    for (case, want) in [
        (0, "delivery state missing"),
        (1, "mixed delivery payload owners"),
        (2, "unknown private schema selection"),
        (3, "invalid delivery policy lineage"),
        (4, "invalid delivery metadata: version or store incarnation"),
        (5, "invalid baseline generation"),
        (6, "original baseline cut is not retained"),
        (
            7,
            "baseline generation does not name its exact retained roots",
        ),
        (8, "unresolved immutable publication reference"),
        (9, "delivery barrier mismatch"),
    ] {
        let mut state = store.state.clone();
        state.bound.as_mut().unwrap().peer = vec![0];
        state.generation += 1; // A later barrier/restore error must not mask earlier checks.
        match case {
            0 => state.delivery = None,
            1 => state.version = 4,
            2 => state.delivery.as_mut().unwrap().prior_private_schema = 9,
            3 => {
                let d = state.delivery.as_mut().unwrap();
                d.policy_lineage = 0;
                d.ledger.version = 2;
            }
            4 => {
                let d = state.delivery.as_mut().unwrap();
                d.ledger.version = 2;
                d.baseline_generation = 0;
            }
            5 => state.delivery.as_mut().unwrap().baseline_generation = 0,
            6 => {
                let d = state.delivery.as_mut().unwrap();
                assert!(!d.ledger.replay.contains_key(&1));
                d.baseline_generation = 1;
            }
            7 => {
                let d = state.delivery.as_mut().unwrap();
                d.baseline[0].kind = match d.baseline[0].kind {
                    Retention::Checkpoint => Retention::Delta,
                    Retention::Delta => Retention::Checkpoint,
                };
            }
            8 => state.delivery.as_mut().unwrap().tail.push(PublicationRef {
                payload: [0; 32],
                kind: Retention::Delta,
            }),
            9 => (),
            _ => unreachable!(),
        }
        let failure = error(&state, now);
        assert_eq!(failure.to_string(), want, "case {case}");
        if matches!(case, 4 | 8) {
            assert!(
                failure.downcast_ref::<CommitError>().is_some(),
                "preserve original typed codec failure"
            );
        }
    }
    // Preserve this helper's original no-delivery branch; outer schema selection
    // remains Store's responsibility and does not gain acceptance from this test.
    for version in [4, 5, 42] {
        let mut state = store.state.clone();
        state.version = version;
        state.delivery = None;
        state.bound = None;
        delivery::validate(&state, now).unwrap();
    }
}
#[test]
fn cloud_delivery_validation_preserves_v5_then_revision_then_replay_order() {
    let (_, store, v4, now) = fixture();
    let mut state = store.state.clone();
    state.delivery.as_mut().unwrap().policy_revision = "0".repeat(64);
    latest_mut(&mut state).digest[0] ^= 1;
    state.bound.as_mut().unwrap().peer = v4;
    assert_eq!(
        error(&state, now).to_string(),
        "native admission v5 required"
    );
    state.bound.as_mut().unwrap().peer = store.state.bound.as_ref().unwrap().peer.clone();
    assert_eq!(
        error(&state, now).to_string(),
        "delivery policy revision mismatch"
    );
    state.delivery.as_mut().unwrap().policy_revision = store
        .state
        .delivery
        .as_ref()
        .unwrap()
        .policy_revision
        .clone();
    assert_eq!(
        error(&state, now).to_string(),
        "invalid Cloud replay evidence"
    );
}

#[test]
fn cloud_delivery_validation_shared_replay_uses_real_bounded_index_without_heap() {
    use automerge::resource_limits::NativeAllowance;
    use qos_peer_state::{BoundedStatePeer, PeerLimits, PeerRegistry};
    let (_, store, _, now) = fixture();
    let mut unknown = store.state.clone();
    latest_mut(&mut unknown).admitted_operations[0] = [231; 32];
    rehash_latest(&mut unknown);
    let mut malformed_room = store.state.clone();
    malformed_room.room = "not-a-room-uuid".into();
    let bound = store.state.bound.as_ref().unwrap();
    let revocations = crate::revocations(&bound.revocations).unwrap();
    let roster = crate::active_roster(&bound.roster, bound.root, &revocations, now).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let peer = BoundedStatePeer::restore(
        &bound.peer,
        &store.state.room,
        &bound.authority,
        &roster,
        &revocations,
        now_time(now),
        &mut work,
    )
    .unwrap();
    let read = peer.read(&work).unwrap();
    assert!(read.admission_v5_selected());
    assert!(!read.contains_admitted_operation(&[231; 32]));
    let _exhausted = work.reserve_output(current.available_bytes()).unwrap();
    let baseline = current.retained_bytes();
    for (state, valid) in [(&store.state, true), (&unknown, false)] {
        let outcome = crate::delivery::resources::precharged_test(&current, || {
            super::replay(
                state,
                state.delivery.as_ref().unwrap(),
                super::Admissions::Bounded(&read),
            )
        });
        if valid {
            outcome.unwrap();
        } else {
            assert!(matches!(
                outcome,
                Err(super::CheckError::Invalid("invalid Cloud replay evidence"))
            ));
        }
        assert_eq!(current.retained_bytes(), baseline);
    }
    let outcome = crate::delivery::resources::precharged_test(&current, || {
        super::replay(
            &malformed_room,
            malformed_room.delivery.as_ref().unwrap(),
            super::Admissions::Bounded(&read),
        )
    });
    let error = outcome.unwrap_err().into_legacy();
    assert!(
        error.downcast_ref::<uuid::Error>().is_some(),
        "preserve original streaming room error"
    );
}

use crate::delivery::resources::{
    AggregateBytes, CodecBudget, DecodeLimits, DeliveryStateFailure, HeldCloudState,
    precharged_test,
};
use automerge::resource_limits::NativeAllowance;
use qos_peer_state::{BoundedStatePeer, PeerLimits, PeerRegistry, PeerWork};

// Signed Store/native fixture creation and serde fixture bytes are outside the
// measured operation. Every held decode, private check, native restore and
// delivery check below uses the same actual PeerWork/output ledger.
fn with_held<T>(
    state: &State,
    now: u64,
    action: impl FnOnce(&HeldCloudState, &BoundedStatePeer, &PeerWork<'_>, &NativeAllowance) -> T,
) -> T {
    let bytes = serde_json::to_vec(state).unwrap();
    let mut registry = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let initial_credit = current.retained_bytes();
    let mut turn = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = turn.work().unwrap();
    let (input, held) = precharged_test(&current, || {
        let budget = CodecBudget::peer(&work);
        let input = AggregateBytes::copy_from(&bytes, bytes.len(), &budget).unwrap();
        let held = HeldCloudState::decode(
            &input,
            DecodeLimits {
                maximum_bytes: 64 * 1024 * 1024,
                allocation_bytes: 8 * 1024 * 1024,
                collection_entries: 1024 * 1024,
            },
            &budget,
        )
        .unwrap();
        (input, held)
    });
    precharged_test(&current, || held.validate_private(&work)).unwrap();
    let bound = held.state().bound.as_ref().unwrap();
    // The parent has its own paid verified-roster path; these fixture inputs
    // exercise the original verification without claiming that parent hook.
    let revocations = crate::revocations(&bound.revocations).unwrap();
    let roster = crate::active_roster(&bound.roster, bound.root, &revocations, now).unwrap();
    let peer = precharged_test(&current, || {
        BoundedStatePeer::restore(
            &bound.peer,
            &held.state().room,
            &bound.authority,
            &roster,
            &revocations,
            now_time(now),
            &mut work,
        )
    })
    .unwrap();
    let result = action(&held, &peer, &work, &current);
    assert_eq!(serde_json::to_vec(held.state()).unwrap(), bytes);
    drop((peer, held, input));
    drop(work);
    // Native backing registrations reconcile when the real peer turn ends.
    drop(turn);
    drop(registry);
    assert_eq!(current.retained_bytes(), initial_credit);
    result
}
fn measured_delivery(
    held: &HeldCloudState,
    peer: &BoundedStatePeer,
    work: &PeerWork<'_>,
    current: &NativeAllowance,
) -> Result<(), DeliveryStateFailure> {
    let baseline = current.retained_bytes();
    let result = precharged_test(current, || held.validate_delivery(peer, work));
    assert_eq!(current.retained_bytes(), baseline);
    result
}
fn consumers(state: &mut State, count: u8) {
    let root = state.bound.as_ref().unwrap().root;
    let ledger = &mut state.delivery.as_mut().unwrap().ledger;
    for byte in 1..=count {
        ledger.consumers.insert(
            [byte; 32],
            qos_relay::delivery_v3::commit::ConsumerState {
                id: [byte; 16],
                device: [byte; 32],
                root,
                authorization_view: [3; 32],
                acknowledged: None,
            },
        );
    }
}
#[test]
fn cloud_delivery_validation_held_actual_peer_and_private_versions_are_paid() {
    for private in 0..3 {
        let (_dir, mut store, _, _) = fixture();
        let device = ed25519_dalek::SigningKey::from_bytes(&[8; 32])
            .verifying_key()
            .to_bytes();
        let caller = store.agent_caller(device).unwrap();
        match private {
            0 => (),
            1 => store.activate_private_agent_ledger(&caller).unwrap(),
            2 => store.activate_private_agent_runtime_v2(&caller).unwrap(),
            _ => unreachable!(),
        }
        let now = qos_relay::clock::now_ns();
        let mut state = store.state.clone();
        // Inert persisted consumer fixtures exercise the real duplicate-check
        // set allocation. They are not certified or authorized consumers.
        consumers(&mut state, 128);
        delivery::validate(&state, now).unwrap();
        with_held(&state, now, |held, peer, work, current| {
            assert_eq!(measured_delivery(held, peer, work, current), Ok(()));
        });
    }
}
#[test]
fn cloud_delivery_validation_held_rejects_semantic_corruption_and_failed_basis() {
    let (_dir, store, v4, now) = fixture();
    for case in 0..5 {
        let mut state = store.state.clone();
        consumers(&mut state, 64);
        let want = match case {
            0 => {
                state.delivery.as_mut().unwrap().policy_revision = "0".repeat(64);
                "delivery policy revision mismatch"
            }
            1 => {
                latest_mut(&mut state).digest[0] ^= 1;
                "invalid Cloud replay evidence"
            }
            2 => {
                latest_mut(&mut state).admitted_operations[0] = [231; 32];
                rehash_latest(&mut state);
                "invalid Cloud replay evidence"
            }
            3 => {
                state.bound.as_mut().unwrap().peer = v4.clone();
                "native admission v5 required"
            }
            4 => {
                let bound = state.bound.as_ref().unwrap();
                let mut empty = crate::StatePeer::new(
                    &state.room,
                    bound.authority.clone(),
                    vec![bound.authority.clone()],
                    qos_user_identity::RevocationList::new(),
                    now_time(now),
                )
                .unwrap();
                empty.select_admission_v5().unwrap();
                state.bound.as_mut().unwrap().peer = empty.durable().unwrap();
                "private policy room unavailable"
            }
            _ => unreachable!(),
        };
        state.delivery.as_ref().unwrap().validate().unwrap();
        let legacy = error(&state, now);
        assert_eq!(legacy.to_string(), want);
        with_held(&state, now, |held, peer, work, current| {
            if case == 2 {
                assert!(
                    !peer
                        .read(work)
                        .unwrap()
                        .contains_admitted_operation(&[231; 32])
                );
            }
            if case == 4 {
                assert!(peer.read(work).unwrap().admission_v5_selected());
                assert!(matches!(
                    precharged_test(current, || peer.read(work).unwrap().private_policy_basis()),
                    Err(qos_peer_state::BoundedPeerError::Invalid(_))
                ));
            }
            assert_eq!(
                measured_delivery(held, peer, work, current),
                Err(DeliveryStateFailure::Denied),
                "case {case}"
            );
        });
    }
}
#[test]
fn cloud_delivery_validation_held_original_ledger_exhaustion_and_order() {
    let (_dir, store, v4, now) = fixture();
    for case in 0..4 {
        let mut state = store.state.clone();
        match case {
            0 => (),
            1 => state.generation += 1,
            2 => state.bound.as_mut().unwrap().peer = v4.clone(),
            3 => state.outbox.push(crate::StoredPublication {
                message: crate::NeighbourMsg::SyncResp {
                    room: state.room.clone(),
                    changes: vec![],
                },
                kind: crate::PublicationKind::Delta,
            }),
            _ => unreachable!(),
        }
        with_held(&state, now, |held, peer, work, current| {
            let all = work.reserve_output(current.available_bytes()).unwrap();
            assert_eq!(
                measured_delivery(held, peer, work, current),
                Err(if case == 0 {
                    DeliveryStateFailure::Capacity
                } else {
                    DeliveryStateFailure::Denied
                }),
                "case {case}: earlier semantic refusal must precede paid policy projection"
            );
            drop(all);
            if case == 0 {
                assert_eq!(measured_delivery(held, peer, work, current), Ok(()));
            }
        });
    }
    let mut state = store.state.clone();
    consumers(&mut state, 128);
    with_held(&state, now, |held, peer, work, current| {
        // Leave less than the concrete consumer-set scratch reservation while
        // retaining the actual native/aggregate allocations on this same ledger.
        let all = work.reserve_output(current.available_bytes() - 1).unwrap();
        assert_eq!(
            measured_delivery(held, peer, work, current),
            Err(DeliveryStateFailure::Capacity)
        );
        drop(all);
        assert_eq!(measured_delivery(held, peer, work, current), Ok(()));
    });
}
#[test]
fn cloud_delivery_validation_held_refuses_foreign_domain_and_sealed_work() {
    let (_dir, store, _, now) = fixture();
    with_held(&store.state, now, |held, peer, work, current| {
        let mut foreign = PeerRegistry::new(NativeAllowance::new(4096).unwrap()).unwrap();
        let other = NativeAllowance::new(8 * 1024 * 1024).unwrap();
        let mut turn = foreign
            .begin_turn(other.clone(), PeerLimits::default())
            .unwrap();
        let foreign_work = turn.work().unwrap();
        assert_eq!(
            measured_delivery(held, peer, &foreign_work, &other),
            Err(DeliveryStateFailure::Denied)
        );
        assert_eq!(measured_delivery(held, peer, work, current), Ok(()));
        current.finish_turn();
        assert_eq!(
            measured_delivery(held, peer, work, current),
            Err(DeliveryStateFailure::Capacity)
        );
    });
}
