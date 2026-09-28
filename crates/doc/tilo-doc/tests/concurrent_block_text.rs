//! A block's text is created with the block, so replicas that start typing
//! into the same never-written block concurrently share one character-level
//! sequence instead of racing to create conflicting `text` objects (where
//! Automerge would expose only one winner and hide the other paragraph).

use automerge::{ActorId, AutoCommit, ROOT};
use tilo_automerge::ApplyIncoming;
use tilo_doc::model::{self, NoteKind, Region};
use tilo_doc::{Containers, DocMut};

fn with_dm<R>(doc: &mut AutoCommit, f: impl FnOnce(&mut DocMut<'_>) -> R) -> R {
    let c = model::open(doc, &ROOT).unwrap();
    let mut ids = model::seeded_id_gen(doc, &c);
    let mut dm = DocMut {
        doc,
        c: &c,
        ids: &mut ids,
    };
    let out = f(&mut dm);
    doc.commit();
    out
}

fn replicas(base: &mut AutoCommit) -> (AutoCommit, AutoCommit) {
    let a = AutoCommit::load(&base.save()).unwrap();
    let mut b = AutoCommit::load(&base.save()).unwrap();
    b.set_actor(ActorId::random());
    (a, b)
}

fn merged(mut a: AutoCommit, mut b: AutoCommit) -> (AutoCommit, Containers) {
    a.merge_incoming(&mut b).unwrap();
    let c = model::open(&a, &ROOT).unwrap();
    (a, c)
}

fn assert_both(text: &str) {
    assert!(text.contains("left") && text.contains("right"), "{text:?}");
}

#[test]
fn concurrent_first_edits_share_one_text_in_every_block_kind() {
    let mut base = AutoCommit::new();
    let c = model::init(&mut base, &ROOT);
    let mut ids = model::seeded_id_gen(&base, &c);
    let (body, header, note, note_block, comment, comment_block) = {
        let mut dm = DocMut {
            doc: &mut base,
            c: &c,
            ids: &mut ids,
        };
        let body = dm.insert_block(None, "paragraph");
        let header = dm.insert_block_in(Region::Header, None, "paragraph");
        let note = dm.insert_note(NoteKind::Footnote);
        let note_block = dm.insert_note_block(&note, None, "paragraph").unwrap();
        let comment = dm.insert_comment("x", "2026-01-01");
        let comment_block = dm
            .insert_comment_block(&comment, None, "paragraph")
            .unwrap();
        (body, header, note, note_block, comment, comment_block)
    };
    base.commit();
    let (mut a, mut b) = replicas(&mut base);
    for (doc, word) in [(&mut a, "left"), (&mut b, "right")] {
        with_dm(doc, |dm| {
            dm.splice_text(&body, 0, 0, word);
            dm.splice_text_in(Region::Header, &header, 0, 0, word);
            dm.splice_note_text(&note, &note_block, 0, 0, word);
            dm.splice_comment_text(&comment, &comment_block, 0, 0, word);
        });
    }
    let (doc, c) = merged(a, b);
    assert_both(&model::block_text(&doc, &c, &body).unwrap());
    let header_text = &model::materialize_region(&doc, &c, Region::Header)[0].text;
    assert_both(header_text);
    assert_both(&model::materialize_note_blocks(&doc, &c, &note)[0].text);
    assert_both(&model::materialize_comment_blocks(&doc, &c, &comment)[0].text);
}
