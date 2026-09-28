use super::*;
use crate::{
    doc::{ChatDoc, NewMessage},
    model::ChannelKind,
};
use automerge::{ROOT, transaction::Transactable};
fn fixture(texts: &[&str]) -> AutoCommit {
    let mut doc = AutoCommit::new();
    ChatDoc::insert_channel(&mut doc, "one", "ws", "cat", "One", ChannelKind::Text);
    ChatDoc::insert_channel(&mut doc, "two", "ws", "cat", "Two", ChannelKind::Text);
    for (i, text) in texts.iter().enumerate() {
        ChatDoc::push_message_with_id(
            &mut doc,
            "one",
            &format!("m{i}"),
            NewMessage {
                author: "Ada".into(),
                ts: 17,
                text: text.to_string(),
                ..Default::default()
            },
        );
    }
    ChatDoc::push_message_with_id(
        &mut doc,
        "two",
        "private-other",
        NewMessage {
            text: "other channel".into(),
            ..Default::default()
        },
    );
    doc.commit();
    doc
}
fn limits(items: usize) -> Limits {
    Limits {
        items,
        json_bytes: 4096,
        scan_entries: 100_000,
    }
}
#[test]
fn semantic_read_exact_channel_and_requested_message_capacity() {
    let doc = fixture(&["one", "two", "three"]);
    let page = read_messages(&doc, &doc.base_heads(), "one", None, limits(2)).unwrap();
    assert_eq!(
        page.messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(page.messages[0].author, "Ada");
    assert_eq!(page.messages[0].ts, 17);
    assert_eq!(
        read_messages(
            &doc,
            &doc.base_heads(),
            "two",
            page.next.as_ref(),
            limits(2)
        )
        .unwrap_err(),
        ReadError::InvalidCursor
    );
    let last = read_messages(
        &doc,
        &doc.base_heads(),
        "one",
        page.next.as_ref(),
        limits(2),
    )
    .unwrap();
    assert_eq!(
        last.messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["three"]
    );
    assert!(last.next.is_none());
    assert_eq!(
        read_messages(&doc, &doc.base_heads(), "absent", None, limits(2)).unwrap_err(),
        ReadError::Malformed
    );
}
#[test]
fn semantic_read_long_message_fragments_are_json_bounded_and_lossless() {
    let text = "\"\n界🦀".repeat(4000);
    let doc = fixture(&[&text, "tail"]);
    let heads = doc.base_heads();
    let mut cursor = None;
    let mut reconstructed = String::new();
    for _ in 0..100 {
        let page =
            read_messages(&doc, &doc.base_heads(), "one", cursor.as_ref(), limits(32)).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
        for message in &page.messages {
            reconstructed.push_str(&message.text);
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none());
    assert_eq!(reconstructed, format!("{text}tail"));
    assert_eq!(doc.base_heads(), heads);
    assert_eq!(doc.pending_ops(), 0);
}
#[test]
fn semantic_read_pending_stale_and_work_exhaustion_do_not_commit() {
    let mut doc = fixture(&["a", "b"]);
    let page = read_messages(&doc, &doc.base_heads(), "one", None, limits(1)).unwrap();
    let before = doc.base_heads();
    doc.put(ROOT, "pending", true).unwrap();
    let pending = doc.pending_ops();
    assert_eq!(
        read_messages(&doc, &doc.base_heads(), "one", None, limits(1)).unwrap_err(),
        ReadError::Pending
    );
    assert_eq!(doc.pending_ops(), pending);
    assert_eq!(doc.base_heads(), before);
    doc.commit();
    assert_eq!(
        read_messages(
            &doc,
            &doc.base_heads(),
            "one",
            page.next.as_ref(),
            limits(1)
        )
        .unwrap_err(),
        ReadError::StaleCursor
    );
    assert_eq!(
        read_messages(
            &doc,
            &doc.base_heads(),
            "one",
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
    let (page, largest) =
        allocation::largest(|| read_messages(&doc, &owner_heads, "one", None, limits(32)));
    let page = page.unwrap();
    assert!(page.messages[0].text.len() < 4096);
    assert!(page.next.is_some());
    assert!(largest < 32 * 1024, "whole-content allocation: {largest}");
}

#[test]
fn semantic_read_full_requested_capacity_and_noncanonical_cursor() {
    let doc = fixture(&["short"; 40]);
    let limits = Limits {
        items: 32,
        json_bytes: 65_536,
        scan_entries: 100_000,
    };
    let page = read_messages(&doc, &doc.base_heads(), "one", None, limits).unwrap();
    assert_eq!(page.messages.len(), 32);
    let last = read_messages(&doc, &doc.base_heads(), "one", page.next.as_ref(), limits).unwrap();
    assert_eq!(last.messages.len(), 8);
    assert!(last.next.is_none());
    let mut invalid = page.next.unwrap();
    invalid.position.byte = 1;
    assert_eq!(
        read_messages(&doc, &doc.base_heads(), "one", Some(&invalid), limits).unwrap_err(),
        ReadError::InvalidCursor
    );
}
#[test]
fn semantic_read_preserves_existing_numeric_timestamp_projection() {
    use automerge::ReadDoc;
    let mut doc = fixture(&["hello"]);
    let messages = doc.get(ROOT, "messages").unwrap().unwrap().1;
    let list = doc.get(&messages, "one").unwrap().unwrap().1;
    let message = doc.get(&list, 0).unwrap().unwrap().1;
    doc.put(&message, "ts", 17u64).unwrap();
    doc.commit();
    assert_eq!(
        read_messages(&doc, &doc.base_heads(), "one", None, limits(1))
            .unwrap()
            .messages[0]
            .ts,
        17
    );
}

#[test]
fn semantic_read_marked_text_keeps_only_text_and_real_embedded_atoms_across_pages() {
    use automerge::{
        ReadDoc,
        marks::{ExpandMark, Mark},
    };
    let plain = "0123456789".repeat(1000);
    let mut doc = fixture(&[&plain]);
    let messages = doc.get(ROOT, "messages").unwrap().unwrap().1;
    let list = doc.get(&messages, "one").unwrap().unwrap().1;
    let message = doc.get(&list, 0).unwrap().unwrap().1;
    let text = doc.get(&message, "text").unwrap().unwrap().1;
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
        let page = read_messages(&doc, &heads, "one", cursor.as_ref(), limits(32)).unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 4096);
        for value in &page.messages {
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
