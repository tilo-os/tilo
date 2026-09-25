use super::*;
use automerge::resource_limits::NativeAllowance;
use qos_peer_state::transport::Retention;
use qos_relay::delivery_v3::commit::{CommitRef, PublicationRef};
fn reference(state: &State, store: StoreGeneration, replay: &ReplayCommit) -> Digest {
    let bytes = postcard::to_stdvec(&(
        store,
        &state.room,
        qos_relay::room_workspace_id(&state.room).unwrap(),
        replay.owner_generation,
        &replay.previous,
        &replay.publications,
        &replay.admitted_operations,
    ))
    .unwrap();
    *blake3::Hasher::new_derive_key("qOS Cloud delivery commit v1")
        .update(&bytes)
        .finalize()
        .as_bytes()
}
fn fixture() -> (tempfile::TempDir, crate::Store) {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    (dir, store)
}
#[test]
fn cloud_replay_digest_stream_matches_actual_retained_and_all_reference_fields() {
    let (_dir, store) = fixture();
    let delivery = store.state.delivery.as_ref().unwrap();
    let generation = delivery.store_generation();
    for entry in delivery.ledger().replay.values() {
        assert_eq!(reference(&store.state, generation, entry), entry.digest);
        assert_eq!(
            exact(&store.state, generation, entry).unwrap(),
            entry.digest
        );
    }
    // Inert varied encoding cases, not fabricated admission evidence.
    let mut entry = delivery.ledger().replay.values().next().unwrap().clone();
    entry.owner_generation = 129;
    entry.previous = Some(CommitRef {
        sequence: 128,
        digest: [31; 32],
    });
    entry.publications.extend([
        PublicationRef {
            payload: [32; 32],
            kind: Retention::Delta,
        },
        PublicationRef {
            payload: [33; 32],
            kind: Retention::Checkpoint,
        },
    ]);
    entry.admitted_operations.extend([[34; 32], [35; 32]]);
    for store_generation in [[0; 16], [1; 16], [255; 16]] {
        assert_eq!(
            exact(&store.state, store_generation, &entry).unwrap(),
            reference(&store.state, store_generation, &entry)
        );
        let expected = exact(&store.state, store_generation, &entry).unwrap();
        entry.committed_at_ns = entry.committed_at_ns.wrapping_add(1);
        entry.digest = [99; 32];
        assert_eq!(
            exact(&store.state, store_generation, &entry).unwrap(),
            expected,
            "legacy preimage excludes timestamp and its own digest"
        );
        entry.publications.reverse();
        entry.admitted_operations.reverse();
        assert_eq!(
            exact(&store.state, store_generation, &entry).unwrap(),
            reference(&store.state, store_generation, &entry)
        );
    }
}
#[test]
fn cloud_replay_digest_stream_needs_no_heap_encoding() {
    let (_dir, store) = fixture();
    let delivery = store.state.delivery.as_ref().unwrap();
    let entry = delivery.ledger().replay.values().next().unwrap();
    let allowance = NativeAllowance::new(4096).unwrap();
    let _exhausted = allowance.reserve(allowance.available_bytes()).unwrap();
    let digest = crate::delivery::resources::precharged_test(&allowance, || {
        exact(&store.state, delivery.store_generation(), entry)
    });
    assert_eq!(digest.unwrap(), entry.digest);
}
#[test]
fn cloud_replay_digest_stream_refuses_invalid_room_before_output() {
    let (_dir, store) = fixture();
    let delivery = store.state.delivery.as_ref().unwrap();
    let entry = delivery.ledger().replay.values().next().unwrap().clone();
    let generation = delivery.store_generation();
    let mut state = store.state.clone();
    state.room = "invalid".into();
    let allowance = NativeAllowance::new(4096).unwrap();
    let _exhausted = allowance.reserve(allowance.available_bytes()).unwrap();
    let result = crate::delivery::resources::precharged_test(&allowance, || {
        exact(&state, generation, &entry)
    });
    assert!(matches!(result, Err(ReplayDigestError::Room(_))));
}

#[test]
fn cloud_replay_digest_stream_preserves_original_uuid_input_forms() {
    let (_dir, store) = fixture();
    let delivery = store.state.delivery.as_ref().unwrap();
    let generation = delivery.store_generation();
    let entry = delivery.ledger().replay.values().next().unwrap().clone();
    let room = uuid::Uuid::parse_str(&store.state.room).unwrap();
    let canonical = room.to_string();
    let original_hash = blake3::derive_key("tilo-room-transport-v1", canonical.as_bytes());
    let raw = u64::from_le_bytes(original_hash[..8].try_into().unwrap());
    let original_workspace = if raw == 0 { 1 } else { raw };
    let forms = [
        canonical.clone(),
        canonical.to_uppercase(),
        room.simple().to_string(),
        room.braced().to_string(),
    ];
    let mut state = store.state.clone();
    let allowance = NativeAllowance::new(4096).unwrap();
    let _exhausted = allowance.reserve(allowance.available_bytes()).unwrap();
    for form in forms {
        state.room = form;
        let expected = reference(&state, generation, &entry);
        let actual = crate::delivery::resources::precharged_test(&allowance, || {
            (
                qos_relay::room_workspace_id(&state.room).unwrap(),
                exact(&state, generation, &entry).unwrap(),
            )
        });
        assert_eq!(actual, (original_workspace, expected));
    }
    state.room = "invalid".into();
    let old = qos_relay::room_workspace_id(&state.room)
        .unwrap_err()
        .to_string();
    let error = super::super::replay_digest(&state, generation, &entry).unwrap_err();
    assert!(
        error.downcast_ref::<uuid::Error>().is_some(),
        "legacy error retains its original type"
    );
    assert_eq!(error.to_string(), old);
}
