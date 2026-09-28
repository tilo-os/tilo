use crate::{State, Store, delivery, now_time};
use tilo_delivery_protocol::delivery_v3::commit::{CommitError, PublicationRef};
use tilo_peer_state::transport::Retention;

fn fixture() -> (tempfile::TempDir, Store, Vec<u8>, u64) {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    let v4 = store.state.bound.as_ref().unwrap().peer.clone();
    let now = tilo_delivery_protocol::clock::now_ns();
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
    (dir, store, v4, tilo_delivery_protocol::clock::now_ns())
}
fn error(state: &State, now: u64) -> anyhow::Error {
    delivery::validate(state, now).expect_err("invalid retained state must be refused")
}
fn latest_mut(state: &mut State) -> &mut tilo_delivery_protocol::delivery_v3::commit::ReplayCommit {
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
        tilo_user_identity::RevocationList::new(),
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
    for version in [4, 42] {
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
