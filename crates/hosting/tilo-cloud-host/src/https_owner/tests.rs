use super::*;
use crate::{ActionRequest, NativeOperation};

#[test]
fn cloud_https_backend_rejects_foreign_origin_on_the_same_clock_and_store() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
        .unwrap();
    let clock = OwnerPolicyClock::new().unwrap();
    let source = CloudStoreSource::bind_with_clock(store, clock.clone()).unwrap();
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    let another = clock.new_origin();
    let foreign = another.capture().unwrap();
    assert!(source.validate_store(&store, &foreign).is_err());
}

fn source() -> (tempfile::TempDir, CloudStoreSource, [u8; 32]) {
    let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
        .unwrap();
    let source = CloudStoreSource::bind(store).unwrap();
    (dir, source, device)
}

#[test]
fn cloud_https_current_shared_clock_keeps_runtime_floor_across_store_opens() {
    let (_dir, mut store, _) = crate::agent_owner_tests::enrolled();
    let initial = tilo_delivery_protocol::clock::now_ns();
    store.activate_delivery_storage(initial).unwrap();
    let (clock, control) = OwnerPolicyClock::for_test(initial + 2_000_000_000);
    let source = CloudStoreSource::bind_with_clock(store, clock.clone()).unwrap();
    control.set_wall_ns(initial);
    control.advance_monotonic(std::time::Duration::from_nanos(25));
    let Current::Current(time) = source.with_current(|turn| {
        let time = turn.policy_time().unwrap();
        assert!(
            time.belongs_to(&clock),
            "the runtime's exact clock is required"
        );
        assert_eq!(turn.now_ns(), initial + 2_000_000_025);
        assert_eq!(turn.fresh_now_ns().unwrap(), initial + 2_000_000_025);
        time
    }) else {
        panic!("actual selected Store required")
    };
    control.advance_monotonic(std::time::Duration::from_nanos(75));
    assert!(matches!(
        source.with_current(|turn| {
            let next = turn.policy_time().unwrap();
            assert!(next.same_origin(&time));
            assert_eq!(turn.now_ns(), initial + 2_000_000_100);
        }),
        Current::Current(())
    ));
    assert_eq!(time.now_ns().unwrap(), initial + 2_000_000_100);
}

#[test]
fn cloud_https_current_shared_clock_source_denial_retires_retained_origin() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
        .unwrap();
    let clock = OwnerPolicyClock::new().unwrap();
    let source = CloudStoreSource::bind_with_clock(store, clock.clone()).unwrap();
    let Current::Current(time) = source.with_current(|turn| turn.policy_time().unwrap()) else {
        panic!("actual current turn required")
    };
    source.retire();
    assert!(matches!(source.with_current(|_| ()), Current::Denied));
    assert!(
        time.now_ns().is_err(),
        "queued effects lose the retired origin"
    );
    assert!(
        clock.now_ns().is_ok(),
        "runtime clock continues across source retirement"
    );
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    let next = CloudStoreSource::bind_with_clock(store, clock.clone()).unwrap();
    let Current::Current(replacement) = next.with_current(|turn| turn.policy_time().unwrap())
    else {
        panic!("fresh exact Store binding required")
    };
    assert!(replacement.belongs_to(&clock));
    assert!(!replacement.same_origin(&time));
    drop(next);
    assert!(
        replacement.now_ns().is_err(),
        "last source drop retires origin"
    );
}

#[test]
fn cloud_https_current_real_busy_and_fresh_committed_store_turn() {
    let (dir, source, device) = source();
    let mut store = Store::open(dir.path(), "agent-admission").unwrap();
    assert!(matches!(
        source.with_current(|_| panic!("lock contention cannot give current")),
        Current::Busy
    ));
    let before = store.state.generation;
    let room = store.state.room.clone();
    let host = store.key().verifying_key().to_bytes();
    store
        .action(
            device,
            &ActionRequest {
                version: 1,
                room,
                invocation_id: "current-fresh".into(),
                operation: NativeOperation::WriterInsert {
                    text: "fresh current".into(),
                },
            },
            tilo_delivery_protocol::clock::now_ns(),
        )
        .unwrap();
    drop(store);
    let Current::Current((generation, lineage, now)) = source.with_current(|turn| {
        assert_eq!(turn.peer().authority().device_public_key, host);
        assert!(
            matches!(
                crate::delivery::access::try_open(dir.path(), "agent-admission"),
                Err(crate::delivery::access::AccessError::Busy)
            ),
            "actual lock retained through callback"
        );
        (turn.generation(), turn.policy_lineage(), turn.now_ns())
    }) else {
        panic!("actual committed turn required")
    };
    assert!(generation > before);
    assert_eq!(lineage, 1, "document does not change policy");
    assert!(now > 0);
}

#[test]
fn cloud_https_current_turn_origin_and_fresh_clock_are_from_original_source() {
    let (dir, source, _) = source();
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    let other = CloudStoreSource::bind(store).unwrap();
    assert!(matches!(
        source.with_current(|turn| {
            assert!(source.owns_turn(turn));
            assert!(source.clone().owns_turn(turn));
            assert!(
                !other.owns_turn(turn),
                "equal persisted labels cannot substitute live source"
            );
            let first = turn.fresh_now_ns().unwrap();
            let next = turn.fresh_now_ns().unwrap();
            assert!(next >= first && first >= turn.now_ns());
        }),
        Current::Current(())
    ));
}

/// A stopped host whose state file is corrupt never opens, and never
/// reinitializes; restoring the exact bytes opens it again.
#[test]
fn cloud_open_refuses_corrupt_state_while_stopped_and_recovers_its_exact_bytes() {
    let (dir, source, _) = source();
    source.retire();
    drop(source);
    crate::storage::compact(dir.path()).unwrap();
    let path = dir.path().join("state.json");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{broken").unwrap();
    assert!(Store::open(dir.path(), "agent-admission").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{broken", "no repair");
    std::fs::write(&path, &bytes).unwrap();
    let recovered =
        CloudStoreSource::bind(Store::open(dir.path(), "agent-admission").unwrap()).unwrap();
    assert!(matches!(
        recovered.with_current(|_| ()),
        Current::Current(())
    ));
}

/// Another machine's state copied into this host's directory while it is
/// stopped names a host key this directory does not hold: open refuses.
#[test]
fn cloud_open_refuses_state_copied_from_another_host_while_stopped() {
    let (dir, source, _) = source();
    source.retire();
    drop(source);
    crate::storage::compact(dir.path()).unwrap();
    let (replacement, other, _) = crate::agent_owner_tests::enrolled();
    drop(other);
    std::fs::copy(
        dir.path().join("state.json"),
        replacement.path().join("state.json"),
    )
    .unwrap();
    let _ = std::fs::remove_file(replacement.path().join("state.journal"));
    assert!(Store::open(replacement.path(), "agent-admission").is_err());
}

/// A state rolled back to an older generation while the host is stopped,
/// with no peer or relay holding newer state, still opens: nothing outside
/// the state directory witnesses the newest generation. Only a generation
/// witness in a separate custody domain closes this.
#[test]
#[ignore = "open-time generation rollback without any newer peer state is undetected; needs a generation witness outside the state directory"]
fn cloud_open_refuses_generation_rollback_without_newer_peer_state() {
    let (dir, source, device) = source();
    source.retire();
    drop(source);
    let path = dir.path().join("state.json");
    crate::storage::compact(dir.path()).unwrap();
    let old = std::fs::read(&path).unwrap();
    {
        let mut store = Store::open(dir.path(), "agent-admission").unwrap();
        let room = store.state.room.clone();
        store
            .action(
                device,
                &ActionRequest {
                    version: 1,
                    room,
                    invocation_id: "rollback-advance".into(),
                    operation: NativeOperation::WriterInsert {
                        text: "advance".into(),
                    },
                },
                tilo_delivery_protocol::clock::now_ns(),
            )
            .unwrap();
    }
    crate::storage::compact(dir.path()).unwrap();
    std::fs::write(&path, &old).unwrap();
    assert!(Store::open(dir.path(), "agent-admission").is_err());
}
