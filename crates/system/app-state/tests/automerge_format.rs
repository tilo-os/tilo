//! Automerge wire and storage formats are persistent: `state.automerge` files
//! and change hashes outlive the automerge release that wrote them, and peers
//! on different releases sync with each other. The fixtures under
//! `tests/data/automerge-0.8/` were written by automerge 0.8.0 from the
//! document [`build`] produces, so these tests fail if a dependency bump stops
//! reading old documents, hashes a change differently, or changes the bytes a
//! saved document is made of.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::marks::{ExpandMark, Mark};
use automerge::sync::{self, SyncDoc};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{
    ActorId, AutoCommit, AutomergeError, ChangeHash, ObjType, ROOT, ReadDoc, ScalarValue, Value,
};
use tilo_automerge::ApplyIncoming;

static DOCUMENT: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    include_str!("data/automerge-0.8/document.automerge.hex")
        .split_ascii_whitespace()
        .map(|byte| {
            u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
        })
        .collect()
});
static CHANGES: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    include_str!("data/automerge-0.8/changes.automerge.hex")
        .split_ascii_whitespace()
        .map(|byte| {
            u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
        })
        .collect()
});
static SYNC_MESSAGE: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    include_str!("data/automerge-0.8/sync-message.bin.hex")
        .split_ascii_whitespace()
        .map(|byte| {
            u8::from_str_radix(byte, 16).expect("checked-in fixture contains hexadecimal bytes")
        })
        .collect()
});

/// Change hashes automerge 0.8.0 computed for [`build`], in causal order.
const CHANGE_HASHES: [&str; 4] = [
    "6bc806baff68fd81c1e3374260c314d4f9c0a012d254bda1ba1d361b97f9bf92",
    "59cb261ee1c168d66a90bfd047330f647dc184b703d3572031801e49fc4e3f5b",
    "6fca5a3ae5492be37f64342ffa22568723a20d11704fcb6d292cd2450008c57d",
    "5500d6e33d45d9744c95222f913f2b4b676faab13e7d6ea22dcf729022c5dc05",
];
const HEAD: &str = "5500d6e33d45d9744c95222f913f2b4b676faab13e7d6ea22dcf729022c5dc05";

/// The reference document: fixed actors, timestamps and merge order, so its
/// change hashes are reproducible. It covers every scalar type, nested maps
/// and lists, text with a mark, counters, deletes and a concurrent conflict.
fn build() -> Result<AutoCommit, AutomergeError> {
    let mut a = AutoCommit::new().with_actor(ActorId::from([0xaa; 16]));
    a.put(ROOT, "title", "Tilo fixture")?;
    a.put(ROOT, "count", ScalarValue::counter(5))?;
    a.put(ROOT, "int", -42_i64)?;
    a.put(ROOT, "uint", ScalarValue::Uint(7))?;
    a.put(ROOT, "float", 1.5_f64)?;
    a.put(ROOT, "flag", true)?;
    a.put(ROOT, "nothing", ScalarValue::Null)?;
    a.put(ROOT, "bytes", ScalarValue::Bytes(vec![0, 1, 2, 0xff]))?;
    a.put(ROOT, "when", ScalarValue::Timestamp(1_700_000_000_000))?;
    let items = a.put_object(ROOT, "items", ObjType::List)?;
    a.insert(&items, 0, "one")?;
    a.insert(&items, 1, "two")?;
    a.insert(&items, 2, 3_i64)?;
    let nested = a.put_object(ROOT, "nested", ObjType::Map)?;
    a.put(&nested, "key", "value")?;
    let body = a.put_object(ROOT, "body", ObjType::Text)?;
    a.splice_text(&body, 0, 0, "Hello world")?;
    a.commit_with(
        CommitOptions::default()
            .with_message("init")
            .with_time(1_700_000_000),
    );

    let mut b = a.fork().with_actor(ActorId::from([0xbb; 16]));

    a.splice_text(&body, 5, 0, ", dear")?;
    a.mark(
        &body,
        Mark::new("bold".to_string(), true, 0, 5),
        ExpandMark::After,
    )?;
    a.increment(ROOT, "count", 3)?;
    a.put(ROOT, "title", "A title")?;
    a.commit_with(CommitOptions::default().with_time(1_700_000_100));

    b.splice_text(&body, 11, 0, "!")?;
    b.put(ROOT, "title", "B title")?;
    b.delete(&items, 1)?;
    b.increment(ROOT, "count", -1)?;
    b.commit_with(CommitOptions::default().with_time(1_700_000_200));

    a.merge_incoming(&mut b)?;
    a.put(ROOT, "merged", "yes")?;
    a.commit_with(
        CommitOptions::default()
            .with_message("after merge")
            .with_time(1_700_000_300),
    );
    Ok(a)
}

fn hex_heads(doc: &mut AutoCommit) -> Vec<String> {
    doc.get_heads().iter().map(ChangeHash::to_string).collect()
}

#[allow(
    clippy::panic,
    reason = "test helper: a non-scalar value fails the calling test"
)]
fn scalar(doc: &AutoCommit, key: &str) -> ScalarValue {
    match doc.get(ROOT, key).unwrap() {
        Some((Value::Scalar(s), _)) => s.into_owned(),
        other => panic!("{key}: expected a scalar, got {other:?}"),
    }
}

fn assert_reference_content(doc: &AutoCommit) {
    assert_eq!(scalar(doc, "title"), ScalarValue::Str("A title".into()));
    assert_eq!(
        doc.get_all(ROOT, "title").unwrap().len(),
        2,
        "conflict kept"
    );
    assert_eq!(scalar(doc, "count").as_i64(), Some(7));
    assert_eq!(scalar(doc, "int"), ScalarValue::Int(-42));
    assert_eq!(scalar(doc, "uint"), ScalarValue::Uint(7));
    assert_eq!(scalar(doc, "float"), ScalarValue::F64(1.5));
    assert_eq!(scalar(doc, "flag"), ScalarValue::Boolean(true));
    assert_eq!(scalar(doc, "nothing"), ScalarValue::Null);
    assert_eq!(
        scalar(doc, "bytes"),
        ScalarValue::Bytes(vec![0, 1, 2, 0xff])
    );
    assert_eq!(
        scalar(doc, "when"),
        ScalarValue::Timestamp(1_700_000_000_000)
    );
    assert_eq!(scalar(doc, "merged"), ScalarValue::Str("yes".into()));

    let (_, items) = doc.get(ROOT, "items").unwrap().unwrap();
    assert_eq!(doc.length(&items), 2);
    assert_eq!(
        doc.get(&items, 1).unwrap().unwrap().0.as_i64(),
        Some(3),
        "the concurrent delete removed the middle element"
    );
    let (_, nested) = doc.get(ROOT, "nested").unwrap().unwrap();
    assert_eq!(
        doc.get(&nested, "key")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .ok(),
        Some("value".to_string())
    );

    let (_, body) = doc.get(ROOT, "body").unwrap().unwrap();
    assert_eq!(doc.text(&body).unwrap(), "Hello, dear world!");
    let marks = doc.marks(&body).unwrap();
    assert_eq!(marks.len(), 1);
    assert_eq!(
        (marks[0].start, marks[0].end, marks[0].name()),
        (0, 5, "bold")
    );
    assert_eq!(marks[0].value(), &ScalarValue::Boolean(true));
}

#[test]
fn documents_change_logs_and_sync_messages_from_automerge_0_8_still_load() {
    // Phase `documents_saved_by_automerge_0_8_still_load`.
    {
        let mut doc = AutoCommit::load(DOCUMENT.as_slice()).unwrap();
        assert_eq!(hex_heads(&mut doc), [HEAD]);
        assert_reference_content(&doc);
    }
    // Phase `change_logs_written_by_automerge_0_8_still_load`.
    {
        let mut doc = AutoCommit::load(CHANGES.as_slice()).unwrap();
        assert_eq!(hex_heads(&mut doc), [HEAD]);
        assert_reference_content(&doc);
    }
    // Phase `sync_messages_from_automerge_0_8_peers_are_understood`.
    {
        let message = sync::Message::decode(SYNC_MESSAGE.as_slice()).unwrap();
        assert_eq!(
            message
                .heads
                .iter()
                .map(ChangeHash::to_string)
                .collect::<Vec<_>>(),
            [HEAD]
        );

        // An empty peer answering the old peer's opening message asks for
        // everything, and the old peer's document then syncs across.
        let mut old_peer = AutoCommit::load(DOCUMENT.as_slice()).unwrap();
        let mut old_state = sync::State::new();
        let mut new_peer = AutoCommit::new();
        let mut new_state = sync::State::new();
        new_peer
            .receive_incoming_sync(&mut new_state, message)
            .unwrap();
        for _ in 0..8 {
            if let Some(m) = new_peer.sync().generate_sync_message(&mut new_state) {
                old_peer.receive_incoming_sync(&mut old_state, m).unwrap();
            }
            if let Some(m) = old_peer.sync().generate_sync_message(&mut old_state) {
                new_peer.receive_incoming_sync(&mut new_state, m).unwrap();
            }
        }
        assert_eq!(hex_heads(&mut new_peer), [HEAD]);
        assert_reference_content(&new_peer);
    }
}

#[test]
fn rebuilt_documents_keep_their_change_hashes_and_saved_bytes() {
    // Phase `change_hashes_are_unchanged`.
    {
        let mut doc = build().unwrap();
        let hashes: Vec<String> = doc
            .get_changes(&[])
            .iter()
            .map(|c| c.hash().to_string())
            .collect();
        assert_eq!(hashes, CHANGE_HASHES);
        assert_eq!(hex_heads(&mut doc), [HEAD]);
    }
    // Phase `saved_bytes_are_unchanged`.
    {
        let mut doc = build().unwrap();
        assert_eq!(doc.save(), DOCUMENT.as_slice(), "save() output");
        let changes: Vec<u8> = doc
            .get_changes(&[])
            .iter()
            .flat_map(|c| c.raw_bytes().to_vec())
            .collect();
        assert_eq!(changes, CHANGES.as_slice(), "raw change bytes");
    }
}
