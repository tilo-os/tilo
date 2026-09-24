use super::*;
use automerge::resource_limits::{
    NativeAllocationSet, NativeAllowance, NativeCloneCandidate, NativeResourceError,
};
use automerge::{
    ReadDoc,
    marks::{ExpandMark, Mark},
};
fn allowance() -> NativeAllowance {
    NativeAllowance::new(64 * 1024 * 1024).unwrap()
}
fn candidate(doc: &AutoCommit) -> (NativeCloneCandidate<AutoCommit>, NativeAllocationSet) {
    let mut allocations = NativeAllocationSet::new(allowance()).unwrap();
    let turn = allocations.work_turn(allowance()).unwrap();
    let candidate = doc.clone_bounded(&mut allocations, &turn).unwrap();
    (candidate, allocations)
}

#[test]
fn semantic_native_chat_matches_real_marked_pages_and_full_capacity() {
    for large in [false, true] {
        let content = "\"\n界🦀".repeat(700);
        let mut doc = if large {
            fixture(&[&content, "tail"])
        } else {
            fixture(&["short"; 40])
        };
        if large {
            let messages = doc.get(ROOT, "messages").unwrap().unwrap().1;
            let list = doc.get(&messages, "one").unwrap().unwrap().1;
            let message = doc.get(&list, 0).unwrap().unwrap().1;
            let text = doc.get(&message, "text").unwrap().unwrap().1;
            doc.split_block(&text, 17).unwrap();
            doc.mark(
                &text,
                Mark::new("bold".into(), true, 8, 100),
                ExpandMark::Both,
            )
            .unwrap();
            doc.mark(
                &text,
                Mark::new("font".into(), "NOT CONTENT", 50, 200),
                ExpandMark::Both,
            )
            .unwrap();
            doc.commit();
        }
        let heads = doc.base_heads();
        let (candidate, allocations) = candidate(&doc);
        let current = allowance();
        let turn = allocations.work_turn(current.clone()).unwrap();
        let read = candidate.read_unqualified(&turn).unwrap();
        let before = current.available_bytes();
        let limits = Limits {
            items: 32,
            json_bytes: if large { 4096 } else { 65536 },
            scan_entries: 100_000,
        };
        let mut cursor = None;
        for page_index in 0..100 {
            let expected = read_messages(&doc, &heads, "one", cursor.as_ref(), limits).unwrap();
            let (actual, peak, largest) = allocation_meter::precharged(&current, || {
                read_messages_native(&read, "one", cursor.as_ref(), limits)
            });
            let actual = actual.unwrap();
            assert!(peak < 512 * 1024 && largest < 64 * 1024);
            assert_eq!(
                serde_json::to_value(actual.page()).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            assert!(current.available_bytes() < before);
            if !large && page_index == 0 {
                assert_eq!(actual.page().messages.len(), 32);
            }
            if expected.next.is_some() {
                assert_eq!(
                    read_messages_native(&read, "two", expected.next.as_ref(), limits).unwrap_err(),
                    ReadError::InvalidCursor
                );
            }
            cursor = expected.next;
            drop(actual);
            assert_eq!(current.available_bytes(), before);
            if cursor.is_none() {
                break;
            }
        }
        assert!(cursor.is_none());
        assert_eq!(doc.base_heads(), heads);
    }
}
#[test]
fn semantic_native_chat_refuses_work_memory_and_preserves_guard_after_seal() {
    let doc = fixture(&["hello"]);
    let (candidate, allocations) = candidate(&doc);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let hold = current.reserve(current.available_bytes() - 1).unwrap();
    assert!(matches!(
        read_messages_native(&read, "one", None, limits(1)),
        Err(ReadError::Resource(NativeResourceError::WorkBytes))
    ));
    drop(hold);
    let short = Limits {
        scan_entries: 1,
        ..limits(1)
    };
    assert!(read_messages_native(&read, "one", None, short).is_err());
    let page = read_messages_native(&read, "one", None, limits(1)).unwrap();
    current.finish_turn();
    assert!(matches!(
        read_messages_native(&read, "one", None, limits(1)),
        Err(ReadError::Resource(NativeResourceError::WorkBytes))
    ));
    drop(read);
    drop(turn);
    assert_eq!(page.page().messages[0].text, "hello");
    let retained = current.retained_bytes();
    drop(page);
    assert!(current.retained_bytes() < retained);
}

#[test]
fn semantic_native_large_borrowed_atom_never_materializes_whole_text() {
    let mut doc = fixture(&["seed"]);
    let messages = doc.get(ROOT, "messages").unwrap().unwrap().1;
    let list = doc.get(&messages, "one").unwrap().unwrap().1;
    let message = doc.get(&list, 0).unwrap().unwrap().1;
    let text = doc.get(&message, "text").unwrap().unwrap().1;
    doc.splice_text(&text, 0, 4, "").unwrap();
    doc.insert(&text, 0, "x".repeat(200_000)).unwrap();
    doc.commit();
    assert_eq!(doc.text(&text).unwrap().len(), 200_000);
    let expected = read_messages(&doc, &doc.base_heads(), "one", None, limits(32)).unwrap();
    let (candidate, allocations) = candidate(&doc);
    let current = allowance();
    let turn = allocations.work_turn(current.clone()).unwrap();
    let read = candidate.read_unqualified(&turn).unwrap();
    let (actual, _, largest) = allocation_meter::precharged(&current, || {
        read_messages_native(&read, "one", None, limits(32))
    });
    assert!(largest < 32 * 1024, "copied whole borrowed atom: {largest}");
    assert_eq!(
        serde_json::to_value(actual.unwrap().page()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}
