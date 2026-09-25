use super::*;
use crate::delivery::resources::CodecBudget;
use automerge::resource_limits::NativeWorkTurn;
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};

pub(super) fn work(bytes: usize) -> (NativeAllowance, NativeWorkTurn) {
    let allowance = NativeAllowance::new(bytes).unwrap();
    let mut allocations = NativeAllocationSet::new(allowance.clone()).unwrap();
    let turn = allocations.work_turn(allowance.clone()).unwrap();
    (allowance, turn)
}
#[test]
fn cloud_aggregate_resources_copy_exact_limit_and_retained_custody() {
    let (allowance, turn) = work(64 * 1024);
    let initial = allowance.available_bytes();
    let held = AggregateBytes::copy_from(
        b"opaque original state bytes",
        27,
        &CodecBudget::native(&turn),
    )
    .unwrap();
    assert_eq!(held.as_bytes(), b"opaque original state bytes");
    assert_eq!(allowance.available_bytes(), initial - 27);
    assert!(matches!(
        AggregateBytes::copy_from(held.as_bytes(), 26, &CodecBudget::native(&turn)),
        Err(ResourceError::Capacity)
    ));
    drop(turn);
    allowance.finish_turn();
    let retained = allowance.retained_bytes();
    drop(held);
    assert_eq!(allowance.retained_bytes(), retained - 27);
}
#[test]
fn cloud_aggregate_resources_counted_output_preserves_literal_schema_bytes() {
    for bytes in [
        include_bytes!("../fixtures/v4.json").as_slice(),
        include_bytes!("../fixtures/v5.json").as_slice(),
    ] {
        let state: State = serde_json::from_slice(bytes).unwrap();
        let expected = serde_json::to_vec(&state).unwrap();
        let (allowance, turn) = work(1024 * 1024);
        let initial = allowance.available_bytes();
        let output = encode_state(&state, expected.len(), &CodecBudget::native(&turn)).unwrap();
        assert_eq!(output.as_bytes(), expected);
        assert_eq!(allowance.available_bytes(), initial - expected.len());
        assert!(matches!(
            encode_state(&state, expected.len() - 1, &CodecBudget::native(&turn)),
            Err(ResourceError::Capacity)
        ));
        drop(output);
        assert_eq!(allowance.available_bytes(), initial);
    }
}
#[test]
fn cloud_aggregate_resources_original_file_is_bounded_before_backing() {
    use std::io::Write;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"original owned file").unwrap();
    let (allowance, turn) = work(64 * 1024);
    let initial = allowance.available_bytes();
    assert!(matches!(
        AggregateBytes::read_file(&mut file, 18, &CodecBudget::native(&turn)),
        Err(ResourceError::Capacity)
    ));
    assert_eq!(allowance.available_bytes(), initial);
    let held = precharged(&allowance, || {
        AggregateBytes::read_file(&mut file, 19, &CodecBudget::native(&turn))
    })
    .unwrap();
    assert_eq!(held.as_bytes(), b"original owned file");
}

// This crate has no other test allocator. Observe actual current-thread heap
// requests against the real caller allowance before every allocation occurs.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
#[derive(Clone, Copy, Default, Debug)]
struct Meter {
    live: usize,
    peak: usize,
    unreserved: usize,
    unmatched: bool,
}
thread_local! {
 static OBSERVED:Cell<Option<Meter>>=const{Cell::new(None)};
 static CREDIT:RefCell<Option<(NativeAllowance,usize)>>=const{RefCell::new(None)};
}
struct Measured;
fn allocated(bytes: usize) {
    let _ = OBSERVED.try_with(|meter| {
        if let Some(mut m) = meter.get() {
            m.live = m.live.checked_add(bytes).unwrap();
            m.peak = m.peak.max(m.live);
            let _ = CREDIT.try_with(|credit| {
                if let Some((allowance, initial)) = &*credit.borrow() {
                    m.unreserved = m.unreserved.max(
                        m.live
                            .saturating_sub(initial.saturating_sub(allowance.available_bytes())),
                    );
                }
            });
            meter.set(Some(m));
        }
    });
}
fn freed(bytes: usize) {
    let _ = OBSERVED.try_with(|meter| {
        if let Some(mut m) = meter.get() {
            // A value's allocation must still be funded when deallocation
            // starts, not merely at its original allocation event.
            let _ = CREDIT.try_with(|credit| {
                if let Some((allowance, initial)) = &*credit.borrow() {
                    m.unreserved = m.unreserved.max(
                        m.live
                            .saturating_sub(initial.saturating_sub(allowance.available_bytes())),
                    );
                }
            });
            match m.live.checked_sub(bytes) {
                Some(n) => m.live = n,
                None => m.unmatched = true,
            };
            meter.set(Some(m));
        }
    });
}
// SAFETY: all allocation operations delegate their exact inputs to System;
// only nonallocating, current-thread counters observe the requested layouts.
unsafe impl GlobalAlloc for Measured {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            allocated(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            allocated(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        freed(l.size());
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let out = unsafe { System.realloc(p, l, n) };
        if !out.is_null() {
            allocated(n);
            freed(l.size());
        }
        out
    }
}
#[global_allocator]
static ALLOCATOR: Measured = Measured;
pub(super) fn precharged<T>(allowance: &NativeAllowance, f: impl FnOnce() -> T) -> T {
    CREDIT.with(|v| *v.borrow_mut() = Some((allowance.clone(), allowance.available_bytes())));
    OBSERVED.with(|v| v.set(Some(Meter::default())));
    let value = f();
    let meter = OBSERVED.with(|v| v.take().unwrap());
    CREDIT.with(|v| *v.borrow_mut() = None);
    assert_eq!(
        meter.unreserved, 0,
        "actual allocation preceded reservation: {meter:?}"
    );
    assert!(
        !meter.unmatched,
        "unexpected preexisting allocation released in measured operation: {meter:?}"
    );
    value
}
#[test]
fn cloud_aggregate_resources_map_value_and_error_output_are_precharged() {
    let mut state: State = serde_json::from_slice(include_bytes!("../fixtures/v4.json")).unwrap();
    for i in 0..128 {
        state.consumed.insert(format!("id{i}"),("exact digest".into(),serde_json::json!({"text":"é\\\"\n".repeat(64),"nested":[1,2,{"retained":true}],"number":1e100})));
    }
    let expected = serde_json::to_vec(&state).unwrap();
    let (allowance, turn) = work(2 * 1024 * 1024);
    let output = precharged(&allowance, || {
        encode_state(&state, expected.len(), &CodecBudget::native(&turn))
    })
    .unwrap();
    assert_eq!(output.as_bytes(), expected);
    let error = precharged(&allowance, || {
        encode_state(&state, expected.len() - 1, &CodecBudget::native(&turn))
    });
    assert!(matches!(error, Err(ResourceError::Capacity)));
    let copy = precharged(&allowance, || {
        AggregateBytes::copy_from(
            output.as_bytes(),
            expected.len(),
            &CodecBudget::native(&turn),
        )
    })
    .unwrap();
    assert_eq!(copy.as_bytes(), expected);
}
#[test]
fn cloud_aggregate_resources_delivery_and_private_versions_preserve_canonical_output_and_budget() {
    for private in [1, 2] {
        let (_dir, mut store, device) = crate::agent_owner_tests::enrolled();
        let caller = store.agent_caller(device).unwrap();
        if private == 1 {
            store.activate_private_agent_ledger(&caller).unwrap();
        } else {
            store.activate_private_agent_runtime_v2(&caller).unwrap();
        }
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
        let delivery = store.state.delivery.as_mut().unwrap();
        for i in 1u16..=128 {
            let mut device = [0; 32];
            device[..2].copy_from_slice(&i.to_le_bytes());
            let mut id = [0; 16];
            id[..2].copy_from_slice(&i.to_le_bytes());
            delivery.ledger.consumers.insert(
                device,
                qos_relay::delivery_v3::commit::ConsumerState {
                    id,
                    device,
                    root: [3; 32],
                    authorization_view: [4; 32],
                    acknowledged: None,
                },
            );
        }
        let expected = serde_json::to_vec(&store.state).unwrap();
        let (allowance, turn) = work(16 * 1024 * 1024);
        let initial = allowance.available_bytes();
        let output = precharged(&allowance, || {
            encode_state(&store.state, expected.len(), &CodecBudget::native(&turn))
        })
        .unwrap();
        assert_eq!(output.as_bytes(), expected);
        assert_eq!(
            allowance.available_bytes(),
            initial - expected.len(),
            "temporary nested buffers/set reservations must end after encode"
        );
        drop(output);
        assert_eq!(allowance.available_bytes(), initial);
    }
}

#[test]
fn cloud_aggregate_resources_exhausted_inventory_refuses_before_allocation() {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"original owned file").unwrap();
    let state: State = serde_json::from_slice(include_bytes!("../fixtures/v4.json")).unwrap();
    let (allowance, turn) = work(64 * 1024);
    let initial = allowance.available_bytes();
    let occupied = turn.reserve(initial).unwrap();
    assert_eq!(allowance.available_bytes(), 0);
    assert!(matches!(
        precharged(&allowance, || AggregateBytes::copy_from(
            b"x",
            1,
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::Capacity)
    ));
    assert!(matches!(
        precharged(&allowance, || AggregateBytes::read_file(
            &mut file,
            19,
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::Capacity)
    ));
    assert!(matches!(
        precharged(&allowance, || encode_state(
            &state,
            64 * 1024,
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::Capacity)
    ));
    drop(occupied);
    assert_eq!(allowance.available_bytes(), initial);
    let bytes = precharged(&allowance, || {
        AggregateBytes::read_file(&mut file, 19, &CodecBudget::native(&turn))
    })
    .unwrap();
    assert_eq!(bytes.as_bytes(), b"original owned file");
    drop(bytes);
    assert_eq!(allowance.available_bytes(), initial);
}
#[test]
fn cloud_aggregate_resources_unknown_schema_and_empty_original_refuse_without_custody() {
    let mut state: State = serde_json::from_slice(include_bytes!("../fixtures/v4.json")).unwrap();
    state.version = 7;
    let mut empty = tempfile::tempfile().unwrap();
    let (allowance, turn) = work(64 * 1024);
    let initial = allowance.available_bytes();
    assert!(matches!(
        precharged(&allowance, || encode_state(
            &state,
            64 * 1024,
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::UnsupportedSchema)
    ));
    assert!(matches!(
        precharged(&allowance, || AggregateBytes::copy_from(
            b"",
            64 * 1024,
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::Malformed)
    ));
    assert!(matches!(
        precharged(&allowance, || AggregateBytes::read_file(
            &mut empty,
            64 * 1024,
            &CodecBudget::native(&turn)
        )),
        Err(ResourceError::Malformed)
    ));
    assert_eq!(allowance.available_bytes(), initial);
}
