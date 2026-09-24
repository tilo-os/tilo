use super::super::{AggregateBytes, tests::precharged};
use super::*;
use automerge::resource_limits::NativeAllowance;
use qos_peer_state::{PeerLimits, PeerRegistry};
#[test]
fn cloud_aggregate_budget_peer_keeps_original_current_credit() {
    let initial = NativeAllowance::new(4096).unwrap();
    let mut registry = PeerRegistry::new(initial.clone()).unwrap();
    let initial_before = initial.retained_bytes();
    let current = NativeAllowance::new(4096).unwrap();
    let baseline = current.retained_bytes();
    let mut operation = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let work = operation.work().unwrap();
    let budget = CodecBudget::peer(&work);
    let input = precharged(&current, || {
        AggregateBytes::copy_from(&[9; 128], 128, &budget)
    })
    .unwrap();
    assert_eq!(initial.retained_bytes(), initial_before);
    let exhausted = budget.reserve(current.available_bytes()).unwrap();
    assert!(matches!(
        precharged(&current, || AggregateBytes::copy_from(b"x", 1, &budget)),
        Err(ResourceError::Capacity)
    ));
    drop(exhausted);
    drop(budget);
    drop(work);
    drop(operation);
    drop(registry);
    assert_eq!(current.retained_bytes(), baseline + 128);
    assert_eq!(input.as_bytes(), &[9; 128]);
    drop(input);
    assert_eq!(current.retained_bytes(), baseline);
}

#[test]
fn cloud_aggregate_budget_peer_reborrow_spans_native_restore_candidate_and_output() {
    use super::super::{DecodeLimits, HeldCloudState, MetadataChange, encode_state};
    use qos_peer_state::BoundedStatePeer;
    use qos_relay::delivery_v3::commit::ConsumerState;
    use std::io::Write;
    // Legacy fixture construction is outside the measured codec/native operation.
    // The adapter itself never constructs another registry, allowance or work.
    let (_root, mut store, device) = crate::agent_owner_tests::enrolled();
    let caller = store.agent_caller(device).unwrap();
    store.activate_private_agent_ledger(&caller).unwrap();
    let (caller, request) = crate::agent_owner_tests::prepare(&store, device, "peer-budget");
    store.commit_agent_request(&request, &caller).unwrap();
    let now = qos_relay::clock::now_ns();
    store.activate_delivery_storage(now).unwrap();
    let bytes = serde_json::to_vec(&store.state).unwrap();
    let peer = crate::delivery::committed_peer(&store.state, now).unwrap();
    let durable = peer.durable().unwrap();
    let roster = peer.certificates();
    let revocations = qos_user_identity::RevocationList::new();
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&bytes).unwrap();
    let limits = DecodeLimits {
        maximum_bytes: 64 * 1024 * 1024,
        allocation_bytes: 8 * 1024 * 1024,
        collection_entries: 1024 * 1024,
    };
    let initial = NativeAllowance::new(4096).unwrap();
    let mut registry = PeerRegistry::new(initial.clone()).unwrap();
    let initial_before = initial.retained_bytes();
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let baseline = current.retained_bytes();
    let mut operation = registry
        .begin_turn(current.clone(), PeerLimits::default())
        .unwrap();
    let mut work = operation.work().unwrap();
    let (input, held) = {
        let budget = CodecBudget::peer(&work);
        let input = precharged(&current, || {
            AggregateBytes::read_file(&mut file, limits.maximum_bytes, &budget)
        })
        .unwrap();
        let held =
            precharged(&current, || HeldCloudState::decode(&input, limits, &budget)).unwrap();
        (input, held)
    };
    assert_eq!(initial.retained_bytes(), initial_before);
    // The immutable codec borrow ends before the actual mutable native phase;
    // this is the identical work value, not a renewed normalized-byte counter.
    let native = BoundedStatePeer::restore(
        &durable,
        peer.room(),
        peer.authority(),
        &roster,
        &revocations,
        crate::now_time(now),
        &mut work,
    )
    .unwrap();
    assert_eq!(native.read(&work).unwrap().room(), peer.room());
    let consumer = ConsumerState {
        id: [19; 16],
        device: [20; 32],
        root: [21; 32],
        authorization_view: [22; 32],
        acknowledged: None,
    };
    let (candidate, output) = {
        let budget = CodecBudget::peer(&work);
        let fork = precharged(&current, || held.fork(limits, &budget)).unwrap();
        let encoded = precharged(&current, || {
            encode_state(fork.state(), limits.maximum_bytes, &budget)
        })
        .unwrap();
        assert_eq!(encoded.as_bytes(), bytes);
        drop(encoded);
        drop(fork);
        let candidate = precharged(&current, || {
            held.metadata_candidate(
                MetadataChange::Consumer(consumer.clone()),
                held.state().generation + 1,
                limits,
                &budget,
            )
        })
        .unwrap()
        .unwrap();
        let output = precharged(&current, || {
            encode_state(candidate.state(), limits.maximum_bytes, &budget)
        })
        .unwrap();
        let exhausted = budget.reserve(current.available_bytes()).unwrap();
        assert!(matches!(
            precharged(&current, || held.fork(limits, &budget)),
            Err(ResourceError::Capacity)
        ));
        assert!(matches!(
            precharged(&current, || HeldCloudState::decode(&input, limits, &budget)),
            Err(ResourceError::Capacity)
        ));
        assert!(matches!(
            precharged(&current, || held.metadata_candidate(
                MetadataChange::Consumer(consumer),
                held.state().generation + 1,
                limits,
                &budget
            )),
            Err(ResourceError::Capacity)
        ));
        assert!(matches!(
            precharged(&current, || encode_state(
                held.state(),
                limits.maximum_bytes,
                &budget
            )),
            Err(ResourceError::Capacity)
        ));
        drop(exhausted);
        assert_eq!(serde_json::to_vec(held.state()).unwrap(), bytes);
        (candidate, output)
    };
    assert_eq!(candidate.state().generation, held.state().generation + 1);
    assert_eq!(
        output.as_bytes(),
        serde_json::to_vec(candidate.state()).unwrap()
    );
    drop(work);
    drop(operation);
    drop(registry);
    drop(native);
    drop(input);
    drop(held);
    assert!(current.retained_bytes() > baseline);
    assert_eq!(
        output.as_bytes(),
        serde_json::to_vec(candidate.state()).unwrap()
    );
    drop(candidate);
    assert_eq!(current.retained_bytes(), baseline + output.as_bytes().len());
    drop(output);
    assert_eq!(current.retained_bytes(), baseline);
}
