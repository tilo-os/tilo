//! A text written at once goes into the op set's columns as runs of like
//! values, not op by op (vendor/automerge/TILO-PATCH.md): a text ten
//! thousand letters long costs the columns no more runs than one of a
//! hundred. The documents it makes, and those it merges into, are byte for
//! byte what crates.io automerge 0.12.0 makes from the same calls.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc, TextEncoding, hydrate};

/// Letters of every width: ASCII, accented, a combining accent, CJK, an
/// emoji and a family joined into one grapheme.
const MIXED: &str =
    "Plain words, café, e\u{301}, 漢字 and 🙂 with 👩\u{200d}👩\u{200d}👧 at the end.\n";

/// The calls both crates make: a block written as one object with its
/// text, a text made empty and then spliced, a splice into the middle of
/// it, and a peer's concurrent writes into both texts, merged both ways.
macro_rules! script {
    ($name:ident, $am:ident, $encoding:ty, $merge:ident) => {
        #[allow(
            clippy::disallowed_methods,
            reason = "upstream automerge is the reference here, called as it ships"
        )]
        fn $name(encoding: $encoding, text: &str) -> (Vec<u8>, Vec<u8>, String, String) {
            use $am::transaction::{CommitOptions, Transactable as _};
            use $am::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc as _, hydrate};
            let commit = |doc: &mut AutoCommit, time: i64| {
                doc.commit_with(CommitOptions::default().with_time(time));
            };
            let mut doc =
                AutoCommit::new_with_encoding(encoding).with_actor(ActorId::from(&[1u8][..]));
            doc.put(ROOT, "title", "notes").unwrap();
            commit(&mut doc, 1);
            let mut peer = doc.fork().with_actor(ActorId::from(&[2u8][..]));

            let mut block = std::collections::HashMap::new();
            block.insert("kind", hydrate::Value::from("paragraph"));
            block.insert("text", hydrate::Value::text(encoding, text));
            let block = doc
                .batch_create_object(&ROOT, "block", &hydrate::Value::Map(block.into()), false)
                .unwrap();
            let spliced = doc.put_object(&ROOT, "spliced", ObjType::Text).unwrap();
            doc.splice_text(&spliced, 0, 0, text).unwrap();
            doc.splice_text(&spliced, 3, 2, "-in the middle-").unwrap();
            commit(&mut doc, 2);

            let theirs = peer.put_object(&ROOT, "spliced", ObjType::Text).unwrap();
            peer.splice_text(&theirs, 0, 0, "the peer's own").unwrap();
            commit(&mut peer, 3);
            peer.$merge(&mut doc).unwrap();
            doc.$merge(&mut peer).unwrap();
            let block_text = doc.get(&block, "text").unwrap().unwrap().1;
            peer.splice_text(&block_text, 1, 0, "inserted by the peer")
                .unwrap();
            commit(&mut peer, 4);
            doc.splice_text(&block_text, 2, 1, "and by the writer")
                .unwrap();
            commit(&mut doc, 5);
            doc.$merge(&mut peer).unwrap();
            peer.$merge(&mut doc).unwrap();

            let written = doc.text(&block_text).unwrap();
            let spliced = doc.get(&ROOT, "spliced").unwrap().unwrap().1;
            let spliced = doc.text(&spliced).unwrap();
            (doc.save(), peer.save(), written, spliced)
        }
    };
}

use tilo_automerge::ApplyIncoming as _;

script!(
    fork_script,
    automerge,
    automerge::TextEncoding,
    merge_incoming
);
script!(
    upstream_script,
    upstream_automerge,
    upstream_automerge::TextEncoding,
    merge
);

#[test]
fn a_new_text_is_the_document_upstream_writes() {
    let encodings = [
        (
            TextEncoding::UnicodeCodePoint,
            upstream_automerge::TextEncoding::UnicodeCodePoint,
        ),
        (
            TextEncoding::Utf8CodeUnit,
            upstream_automerge::TextEncoding::Utf8CodeUnit,
        ),
        (
            TextEncoding::Utf16CodeUnit,
            upstream_automerge::TextEncoding::Utf16CodeUnit,
        ),
        (
            TextEncoding::GraphemeCluster,
            upstream_automerge::TextEncoding::GraphemeCluster,
        ),
    ];
    let long = MIXED.repeat(40);
    for (ours, theirs) in encodings {
        for text in ["plain ascii letters only", MIXED, long.as_str()] {
            let fork = fork_script(ours, text);
            let upstream = upstream_script(theirs, text);
            assert_eq!(fork.2, upstream.2, "{ours:?}: the written text");
            assert_eq!(fork.3, upstream.3, "{ours:?}: the spliced text");
            assert!(fork.0 == upstream.0, "{ours:?}: the writer's document");
            assert!(fork.1 == upstream.1, "{ours:?}: the peer's document");
            let mut loaded = AutoCommit::load(&fork.0).unwrap();
            let mut loaded_upstream = upstream_automerge::AutoCommit::load(&upstream.0).unwrap();
            assert_eq!(
                format!("{:?}", loaded.get_heads()),
                format!("{:?}", loaded_upstream.get_heads()),
                "{ours:?}: the saved document's heads"
            );
        }
    }
}

/// The column runs writing a new text of `letters` letters costs.
fn runs_to_write(letters: usize) -> u64 {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(&[1u8][..]));
    let text: String = "Plain words in a paragraph. "
        .chars()
        .cycle()
        .take(letters)
        .collect();
    let before = automerge::work().column_runs;
    let mut block = std::collections::HashMap::new();
    block.insert(
        "text",
        hydrate::Value::text(TextEncoding::UnicodeCodePoint, &text),
    );
    doc.batch_create_object(&ROOT, "block", &hydrate::Value::Map(block.into()), false)
        .unwrap();
    let spliced = doc.put_object(&ROOT, "spliced", ObjType::Text).unwrap();
    doc.splice_text(&spliced, 0, 0, &text).unwrap();
    let runs = automerge::work().column_runs - before;
    doc.commit_with(CommitOptions::default().with_time(0));
    assert_eq!(doc.text(&spliced).unwrap(), text);
    runs
}

#[test]
fn a_long_new_text_costs_the_columns_no_more_runs_than_a_short_one() {
    let short = runs_to_write(100);
    let long = runs_to_write(10_000);
    assert_eq!(long, short, "runs for 10,000 letters against 100");
}

/// A paragraph as a rich text keeps it: a map holding a text and a list.
fn paragraph(words: &str) -> hydrate::Value {
    let mut block = std::collections::HashMap::new();
    block.insert("kind", hydrate::Value::from("paragraph"));
    block.insert(
        "text",
        hydrate::Value::text(TextEncoding::UnicodeCodePoint, words),
    );
    block.insert(
        "tags",
        hydrate::Value::List(vec![hydrate::Value::from("a"), hydrate::Value::from(2)].into()),
    );
    hydrate::Value::Map(block.into())
}

/// The document `keys` made under one map makes, at once or one by one in
/// key order, after `held` were set there, merged with a peer's concurrent
/// writes into that map.
fn made(keys: &[&str], held: &[&str], at_once: bool) -> Vec<u8> {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(&[1u8][..]));
    let parent = doc.put_object(ROOT, "blocks", ObjType::Map).unwrap();
    for key in held {
        doc.put(&parent, *key, "held").unwrap();
    }
    doc.commit_with(CommitOptions::default().with_time(1));
    let mut peer = doc.fork().with_actor(ActorId::from(&[2u8][..]));
    let values: Vec<hydrate::Value> = keys.iter().map(|key| paragraph(key)).collect();
    if at_once {
        let pairs: Vec<(&str, &hydrate::Value)> = keys.iter().copied().zip(&values).collect();
        let ids = doc.batch_create_objects(&parent, &pairs).unwrap();
        for (key, id) in keys.iter().zip(ids) {
            if keys.iter().filter(|other| *other == key).count() > 1 {
                continue;
            }
            assert_eq!(doc.get(&parent, *key).unwrap().unwrap().1, id, "{key}");
        }
    } else {
        let mut sorted: Vec<(&str, &hydrate::Value)> = keys.iter().copied().zip(&values).collect();
        sorted.sort_by_key(|(key, _)| *key);
        for (key, value) in sorted {
            doc.batch_create_object(&parent, key, value, false).unwrap();
        }
    }
    doc.commit_with(CommitOptions::default().with_time(2));
    peer.put(&parent, "b", "the peer's").unwrap();
    peer.put(&parent, "zz", "the peer's own").unwrap();
    peer.commit_with(CommitOptions::default().with_time(3));
    doc.merge_incoming(&mut peer).unwrap();
    doc.save()
}

#[test]
fn objects_made_side_by_side_are_the_document_each_made_in_turn_makes() {
    for (keys, held) in [
        (&["b", "a", "c"][..], &[][..]),
        (&["p1", "p3", "p2", "p10"], &["p0", "p4"]),
        (&["a", "b"], &["b"]),
        (&["a", "a"], &[]),
    ] {
        assert!(
            made(keys, held, true) == made(keys, held, false),
            "{keys:?} under {held:?}"
        );
    }
}

/// The op-set splices making `objects` objects under a map just made costs.
fn splices_to_make(objects: usize) -> u64 {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(&[1u8][..]));
    let parent = doc.put_object(ROOT, "blocks", ObjType::Map).unwrap();
    let keys: Vec<String> = (0..objects).map(|at| format!("p{at}")).collect();
    let values: Vec<hydrate::Value> = keys.iter().map(|key| paragraph(key)).collect();
    let pairs: Vec<(&str, &hydrate::Value)> =
        keys.iter().map(String::as_str).zip(&values).collect();
    let before = automerge::work().op_set_splices;
    doc.batch_create_objects(&parent, &pairs).unwrap();
    automerge::work().op_set_splices - before
}

#[test]
fn objects_made_side_by_side_cost_two_splices_however_many() {
    assert_eq!(splices_to_make(2), 2);
    assert_eq!(splices_to_make(50), 2);
}

#[test]
fn objects_made_side_by_side_leave_nothing_behind_when_rolled_back() {
    for held in [&[][..], &["p0", "p2", "p9"]] {
        let mut doc = AutoCommit::new().with_actor(ActorId::from(&[1u8][..]));
        let parent = doc.put_object(ROOT, "blocks", ObjType::Map).unwrap();
        for key in held {
            doc.put(&parent, *key, "held").unwrap();
        }
        doc.commit_with(CommitOptions::default().with_time(1));
        let before = doc.save();
        let keys = ["p3", "p1", "p5", "p4"];
        let values: Vec<hydrate::Value> = keys.iter().map(|key| paragraph(key)).collect();
        let pairs: Vec<(&str, &hydrate::Value)> = keys.iter().copied().zip(&values).collect();
        doc.batch_create_objects(&parent, &pairs).unwrap();
        doc.rollback();
        assert!(doc.save() == before, "under {held:?}");
    }
}

/// The runs the columns of the change writing a new text of `letters`
/// letters take in.
fn change_runs_to_commit(letters: usize) -> u64 {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(&[1u8][..]));
    let text: String = "Plain words in a paragraph. "
        .chars()
        .cycle()
        .take(letters)
        .collect();
    let spliced = doc.put_object(ROOT, "spliced", ObjType::Text).unwrap();
    doc.splice_text(&spliced, 0, 0, &text).unwrap();
    let before = automerge::work().change_runs;
    doc.commit_with(CommitOptions::default().with_time(0));
    automerge::work().change_runs - before
}

#[test]
fn a_long_new_text_costs_its_change_no_more_runs_than_a_short_one() {
    assert_eq!(change_runs_to_commit(10_000), change_runs_to_commit(100));
}
