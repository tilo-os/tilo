//! Two people opening "the same" document must end up editing the same
//! document.
//!
//! Every other convergence test in this crate starts from `fork()` — one
//! document, copied. That is the easy half, and it is the half that was
//! covered. The hard half is two replicas that each *created* a document,
//! which is what happens whenever two apps import a file: each writes the
//! schema under its own actor, so `root.blocks` is created twice by ops that
//! never saw each other. Automerge resolves that map-key conflict by keeping
//! one op and shadowing the other, and the shadowed side's whole block tree
//! goes with it — silently, and with the winner decided by an actor-id
//! comparison rather than by anything the two people did.
//!
//! These tests are the record of that: they fail against a schema whose
//! containers are minted per-replica, and pass once the schema comes from one
//! genesis every replica already agrees on.

use qdoc::Document;

/// A content-bearing document, built the way an import builds one.
fn imported(paragraphs: &[&str]) -> Document {
    let mut d = Document::new();
    let mut prev: Option<String> = None;
    for p in paragraphs {
        let id = d.insert_block(prev.as_deref(), "paragraph");
        d.splice_text(&id, 0, 0, p);
        prev = Some(id);
    }
    d
}

fn texts(d: &Document) -> Vec<String> {
    d.materialize().blocks.into_iter().map(|b| b.text).collect()
}

/// The defect, stated as the property that has to hold: merging two
/// independently-created documents keeps **both** sides' content.
///
/// Run repeatedly, because the outcome is decided by a comparison between two
/// random actor ids — a single run that happened to keep our side would read
/// as a pass. Losing even once is the bug.
#[test]
fn independently_created_documents_keep_both_sides_blocks() {
    const ROUNDS: usize = 64;
    let mut lost = 0usize;

    for _ in 0..ROUNDS {
        let mut mine = imported(&["a photograph goes here"]);
        let mut theirs = imported(&["their own opening line"]);

        mine.merge(&mut theirs);
        theirs.merge(&mut mine);

        let m = texts(&mine);
        let t = texts(&theirs);
        if !(m.iter().any(|s| s.contains("photograph")) && m.iter().any(|s| s.contains("their own")))
        {
            lost += 1;
        }
        assert_eq!(m, t, "the two replicas must at least agree with each other");
    }

    assert_eq!(
        lost, 0,
        "{lost}/{ROUNDS} merges of two independently-created documents dropped one \
         side's blocks entirely — whichever replica lost the root-key conflict"
    );
}

/// The same defect from the direction that made it visible: only one side has
/// the picture, so when that side loses there is no image block left to draw.
#[test]
fn a_picture_survives_a_merge_with_a_document_that_has_none() {
    const ROUNDS: usize = 64;
    let mut lost = 0usize;

    for _ in 0..ROUNDS {
        // One copy holds a paragraph and a photograph.
        let mut mine = Document::new();
        let p = mine.insert_block(None, "paragraph");
        mine.splice_text(&p, 0, 0, "the quarterly figures");
        let hash = mine.put_blob(&[0xff, 0xd8, 0xff, 0xe0, 1, 2, 3, 4]);
        let img = mine.insert_block(Some(&p), "image");
        mine.as_mut().set_attr(&img, "blob", hash.as_str());

        // The other opens a file of their own, with no image in it.
        let mut theirs = imported(&["nothing here yet"]);

        mine.merge(&mut theirs);
        theirs.merge(&mut mine);

        let snap = theirs.materialize();
        let has_image = snap.blocks.iter().any(|b| b.kind == "image");
        let has_bytes = snap
            .blocks
            .iter()
            .filter(|b| b.kind == "image")
            .filter_map(|b| b.attrs.get("blob"))
            .any(|v| match v {
                qdoc::AttrVal::Str(h) => theirs.blob(h).is_some(),
                _ => false,
            });
        if !(has_image && has_bytes) {
            lost += 1;
        }
    }

    assert_eq!(
        lost, 0,
        "{lost}/{ROUNDS} merges left the far side without the image block and its \
         bytes — the photograph cannot render because it is not in the merged document"
    );
}

/// A third participant, and a peer that joins after there is already content.
/// Order of arrival must not decide whose paragraph survives.
#[test]
fn a_third_participant_does_not_displace_the_first_two() {
    const ROUNDS: usize = 32;
    let mut lost = 0usize;

    for _ in 0..ROUNDS {
        let mut a = imported(&["from alice"]);
        let mut b = imported(&["from bob"]);
        a.merge(&mut b);
        b.merge(&mut a);

        // A third opens the same file only now, after a and b have content.
        let mut c = imported(&["from carol"]);
        c.merge(&mut a);
        a.merge(&mut c);
        b.merge(&mut a);

        let all = texts(&a);
        let complete = ["alice", "bob", "carol"]
            .iter()
            .all(|who| all.iter().any(|s| s.contains(who)));
        if !complete {
            lost += 1;
        }
        assert_eq!(texts(&a), texts(&b), "a and b converge");
        assert_eq!(texts(&a), texts(&c), "a and c converge");
    }

    assert_eq!(
        lost, 0,
        "{lost}/{ROUNDS} three-way merges dropped at least one participant's content"
    );
}
