use super::*;
use std::collections::BTreeMap;

#[test]
fn map_and_vector_growth_within_the_limit_keeps_the_output() {
    let mut budget = Collections::limited(1024 * 1024);
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
    assert_eq!(map.len(), 40);
    assert_eq!(map.get("a3").map(Vec::len), Some(128));
    assert_eq!(numbers.len(), 65);
    assert_eq!(numbers.last(), Some(&64));
}

#[test]
fn limit_refuses_before_copy_growth_or_overflow() {
    let mut budget = Collections::limited(3);
    assert_eq!(budget.text("four"), Err(Capacity));
    assert_eq!(budget.zeroed(4), Err(Capacity));
    assert_eq!(budget.debit(None), Err(Capacity));
    assert_eq!(budget.text("abc").unwrap(), "abc");
    assert_eq!(budget.text("a"), Err(Capacity));

    let mut budget = Collections::limited(16);
    let mut values: Vec<u64> = Vec::new();
    assert_eq!(budget.push(&mut values, 1), Err(Capacity));
    assert!(values.is_empty());

    let mut unqualified = Collections::unqualified();
    assert_eq!(unqualified.zeroed(4096).unwrap().len(), 4096);
}
