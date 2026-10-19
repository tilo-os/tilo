use super::*;
use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
use tilo_automerge::ApplyIncoming as _;
use ui::tilo_resource::Slot;

fn fixture(texts: &[&str]) -> AutoCommit {
    let mut doc = AutoCommit::new();
    let c = crate::model::init(&mut doc).unwrap();
    let mut ids = tilo_doc::IdGen::new("fixture".into());
    let mut dm = tilo_doc::DocMut {
        doc: &mut doc,
        c: &c,
        ids: &mut ids,
    };
    let mut previous = None;
    for text in texts {
        let id = dm.insert_block(previous.as_deref(), "paragraph");
        dm.splice_text(&id, 0, 0, text);
        previous = Some(id);
    }
    doc.commit();
    AutoCommit::load(&doc.save()).unwrap()
}
fn limits(items: usize) -> Limits {
    Limits {
        items,
        json_bytes: 4096,
        scan_entries: 100_000,
    }
}

#[test]
fn semantic_read_pages_requested_capacity_in_literal_tilo_doc_order() {
    let mut doc = fixture(&["third", "first", "second"]);
    let c = crate::model::open(&doc).unwrap();
    let ids = tilo_doc::model::blocks_in_order(&doc, &c);
    for (id, key) in ids.iter().zip([vec![2], vec![1], vec![1, 0]]) {
        let (_, block) = doc.get(&c.blocks, id.as_str()).unwrap().unwrap();
        Slot::Key(&block, "order")
            .put_scalar(&mut doc, ScalarValue::Bytes(key))
            .unwrap();
    }
    doc.commit();
    let before = doc.base_heads();
    let page = read_text(&doc, &doc.base_heads(), None, limits(2)).unwrap();
    let mut tied = [(ids[1].clone(), "first"), (ids[2].clone(), "second")];
    tied.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        page.segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>(),
        tied.iter().map(|x| x.1).collect::<Vec<_>>()
    );
    let last = read_text(&doc, &doc.base_heads(), page.next.as_ref(), limits(2)).unwrap();
    assert_eq!(
        last.segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>(),
        ["third"]
    );
    assert!(last.next.is_none());
    assert_eq!(doc.base_heads(), before);
    assert_eq!(doc.pending_ops(), 0);
}
#[test]
fn semantic_read_long_escaped_unicode_text_pages_without_loss() {
    let text = "\"\n界🦀".repeat(4000);
    let doc = fixture(&[&text, "tail"]);
    let before = doc.base_heads();
    let mut cursor = None;
    let mut reconstructed = String::new();
    for _ in 0..100 {
        let page = read_text(&doc, &doc.base_heads(), cursor.as_ref(), limits(64)).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
        for segment in &page.segments {
            reconstructed.push_str(&segment.text);
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none());
    assert_eq!(reconstructed, format!("{text}tail"));
    assert_eq!(doc.base_heads(), before);
}
#[test]
fn semantic_read_refuses_pending_stale_and_insufficient_scan() {
    let mut doc = fixture(&["alpha", "beta"]);
    let page = read_text(&doc, &doc.base_heads(), None, limits(1)).unwrap();
    let before = doc.base_heads();
    ui::tilo_resource::write_key(&mut doc, &ROOT, "pending", &true).unwrap();
    let pending = doc.pending_ops();
    assert_eq!(
        read_text(&doc, &doc.base_heads(), None, limits(1)).unwrap_err(),
        ReadError::Pending
    );
    assert_eq!(doc.pending_ops(), pending);
    assert_eq!(doc.base_heads(), before);
    doc.commit();
    assert_eq!(
        read_text(&doc, &doc.base_heads(), page.next.as_ref(), limits(1)).unwrap_err(),
        ReadError::StaleCursor
    );
    assert_eq!(
        read_text(
            &doc,
            &doc.base_heads(),
            None,
            Limits {
                scan_entries: 1,
                ..limits(1)
            }
        )
        .unwrap_err(),
        ReadError::Limit
    );
}

// Track allocations only on this test thread; fixtures are built before measurement.
#[global_allocator]
static ALLOCATOR: tilo_test_env::allocation::Meter = tilo_test_env::allocation::Meter;
use tilo_test_env::allocation;

#[test]
fn semantic_read_does_not_copy_whole_large_text() {
    let text = "x".repeat(200_000);
    let doc = fixture(&[&text]);
    let owner_heads = doc.base_heads();
    let (page, largest) = allocation::largest(|| read_text(&doc, &owner_heads, None, limits(64)));
    let page = page.unwrap();
    assert!(page.segments[0].text.len() < 4096);
    assert!(page.next.is_some());
    assert!(largest < 32 * 1024, "whole-content allocation: {largest}");
}

#[test]
fn semantic_read_full_requested_capacity_and_noncanonical_cursor() {
    let doc = fixture(&["short"; 70]);
    let limits = Limits {
        items: 64,
        json_bytes: 65_536,
        scan_entries: 100_000,
    };
    let page = read_text(&doc, &doc.base_heads(), None, limits).unwrap();
    assert_eq!(page.segments.len(), 64);
    let last = read_text(&doc, &doc.base_heads(), page.next.as_ref(), limits).unwrap();
    assert_eq!(last.segments.len(), 6);
    assert!(last.next.is_none());
    let mut invalid = page.next.unwrap();
    invalid.position.byte = 1;
    assert_eq!(
        read_text(&doc, &doc.base_heads(), Some(&invalid), limits).unwrap_err(),
        ReadError::InvalidCursor
    );
}
#[test]
fn semantic_read_refuses_to_emit_an_unusable_oversized_cursor() {
    let mut doc = fixture(&[]);
    let blocks = crate::model::open(&doc).unwrap().blocks;
    let id = "\"".repeat(256);
    let block = Slot::Key(&blocks, &id)
        .object(&mut doc, automerge::ObjType::Map)
        .unwrap();
    Slot::Key(&block, "order")
        .put_scalar(&mut doc, ScalarValue::Bytes(vec![255u8; 256]))
        .unwrap();
    Slot::Key(&block, "kind")
        .put_scalar(&mut doc, ScalarValue::Str("paragraph".into()))
        .unwrap();
    ui::tilo_resource::write_key(&mut doc, &block, "text", &"x".repeat(6000)).unwrap();
    doc.commit();
    assert_eq!(
        read_text(&doc, &doc.base_heads(), None, limits(1)).unwrap_err(),
        ReadError::Limit
    );
}

#[test]
fn semantic_read_uses_borrowed_owner_heads_without_cloning_legacy_isolation() {
    let mut doc = fixture(&["hello"]);
    let heads = doc.base_heads();
    doc.isolate_at(&vec![heads[0]; 20_000]);
    let (basis, largest) = allocation::largest(|| {
        tilo_doc::semantic_read::source::Source::new(&doc, &heads)
            .begin(limits(1), 64)
            .map(|(basis, _)| basis)
    });
    assert_eq!(basis.unwrap().heads, [heads[0].to_string()]);
    assert!(largest < 4096, "legacy head vector cloned: {largest}");
    assert!(matches!(
        tilo_doc::semantic_read::source::Source::new(&doc, &vec![heads[0]; 65])
            .begin(limits(1), 64),
        Err(ReadError::Limit)
    ));
    assert!(matches!(
        tilo_doc::semantic_read::source::Source::new(&doc, &[heads[0], heads[0]])
            .begin(limits(1), 64),
        Err(ReadError::Malformed)
    ));
}

#[test]
fn semantic_read_marked_text_keeps_only_text_and_real_embedded_atoms_across_pages() {
    use automerge::{
        ReadDoc,
        marks::{ExpandMark, Mark},
    };
    let plain = "0123456789".repeat(1000);
    let mut doc = fixture(&[&plain]);
    let containers = crate::model::open(&doc).unwrap();
    let block = tilo_doc::model::blocks_in_order(&doc, &containers)[0].clone();
    let block = doc.get(&containers.blocks, block).unwrap().unwrap().1;
    let text = doc.get(&block, "text").unwrap().unwrap().1;
    doc.split_block(&text, 17).unwrap();
    doc.mark(
        &text,
        Mark::new("bold".into(), true, 8, 3000),
        ExpandMark::Both,
    )
    .unwrap();
    doc.mark(
        &text,
        Mark::new("font".into(), "NOT BODY TEXT", 1500, 6000),
        ExpandMark::Both,
    )
    .unwrap();
    doc.commit();
    let mut expected = plain;
    expected.insert(17, '\u{fffc}');
    assert_eq!(doc.text(&text).unwrap(), expected);
    // Begin/end for the real overlapping boolean and string marks are list elements with no
    // text width; the reader must skip them.
    assert_eq!(doc.list_range(&text, ..).count(), doc.length(&text) + 4);
    let embedded = doc
        .list_range(&text, ..)
        .filter(|item| matches!(item.value, automerge::ValueRef::Object(_)))
        .count();
    assert_eq!(embedded, 1); // actual block atom remains content
    let heads = doc.base_heads();
    let mut cursor = None;
    let mut result = String::new();
    let mut pages = 0;
    for _ in 0..10 {
        let page = read_text(&doc, &heads, cursor.as_ref(), limits(64)).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
        for value in &page.segments {
            result.push_str(&value.text);
        }
        pages += 1;
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert!(pages > 1);
    assert!(cursor.is_none());
    assert_eq!(result, expected);
    assert_eq!(doc.base_heads(), heads);
    assert_eq!(doc.pending_ops(), 0);
}

#[test]
fn semantic_read_skips_invalid_block_rows_with_oversized_keys() {
    let mut doc = fixture(&["kept"]);
    let blocks = crate::model::open(&doc).unwrap().blocks;
    let scalar_row = "x".repeat(1024);
    Slot::Key(&blocks, &scalar_row)
        .put_scalar(&mut doc, ScalarValue::Str("invalid scalar row".into()))
        .unwrap();
    let map_row = "y".repeat(1024);
    Slot::Key(&blocks, &map_row)
        .object(&mut doc, automerge::ObjType::Map)
        .unwrap();
    doc.commit();
    let page = read_text(&doc, &doc.base_heads(), None, limits(64)).unwrap();
    assert_eq!(page.segments.len(), 1);
    assert_eq!(page.segments[0].text, "kept");
}
