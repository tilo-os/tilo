use super::*;
use crate::native_footprint_vendor_tests::assert_precharged;
use automerge::resource_limits::{NativeAllocationSet, NativeAllowance};
use std::collections::BTreeMap;
struct Held {
    map: BTreeMap<String, Vec<u8>>,
    numbers: Vec<u64>,
    _budget: Collections,
}
#[test]
fn actual_map_and_vector_growth_precharge_and_retain_output_after_turn() {
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let baseline = current.retained_bytes();
    let mut registry = NativeAllocationSet::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let turn = registry.work_turn(current.clone()).unwrap();
    let held = assert_precharged(&current, || {
        let mut budget = Collections::native(&turn, 1024 * 1024).unwrap();
        let mut map = BTreeMap::new();
        for i in 0..40 {
            let key = [b'a' + i / 10, b'0' + i % 10];
            budget.tree_entry::<String, Vec<u8>>().unwrap();
            let key = budget.text(std::str::from_utf8(&key).unwrap()).unwrap();
            let bytes = budget.bytes(&[i; 128]).unwrap();
            map.insert(key, bytes);
        }
        let mut numbers = Vec::new();
        for i in 0..65 {
            budget.push(&mut numbers, i).unwrap();
        }
        Held {
            map,
            numbers,
            _budget: budget,
        }
    });
    assert_eq!(held.map.len(), 40);
    assert_eq!(held.numbers.len(), 65);
    drop(turn);
    current.finish_turn();
    drop(registry);
    assert!(current.retained_bytes() > baseline);
    drop(held);
    assert_eq!(current.retained_bytes(), baseline);
}
#[test]
fn source_budget_refuses_before_copy_growth_or_overflow() {
    let current = NativeAllowance::new(64 * 1024 * 1024).unwrap();
    let mut registry = NativeAllocationSet::new(NativeAllowance::new(4096).unwrap()).unwrap();
    let turn = registry.work_turn(current.clone()).unwrap();
    assert_precharged(&current, || {
        let mut budget = Collections::native(&turn, 3).unwrap();
        assert_eq!(budget.text("four"), Err(NativeResourceError::WorkBytes));
        assert_eq!(budget.zeroed(4), Err(NativeResourceError::WorkBytes));
        assert_eq!(budget.debit(None), Err(NativeResourceError::WorkBytes));
        assert_eq!(budget.text("abc").unwrap(), "abc");
    });
    let all = current.reserve(current.available_bytes()).unwrap();
    assert!(matches!(
        Collections::native(&turn, 1),
        Err(NativeResourceError::WorkBytes)
    ));
    drop(all);
    current.finish_turn();
    assert!(Collections::native(&turn, 0).is_err());
}
