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
