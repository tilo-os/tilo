//! Two people editing the *same paragraph* concurrently must both keep their
//! characters: a paragraph is a character-level CRDT, never a last-writer-wins
//! register.
//!
//! The fault lane found the regression in the product (offline B typing into
//! the first paragraph of a fresh Writer document while A typed into the same
//! paragraph): after the merge only one side's text survived. The cause was
//! in the qdoc model, not the renderer: a new block carried no `text` object,
//! and the first splice on each replica created one lazily with
//! `put_object(block, "text", Text)`. Two concurrent creations are a map-key
//! conflict, so Automerge exposes only one winner and the loser's whole
//! paragraph disappears. These tests drive the canonical Writer actions (the
//! path typing, Ctrl+K and agents share) on two replicas and merge both ways.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ReadDoc};
use tilo_actions::Action;
use word_processor::actions::{
    MarkVal, RegionKind, Span, WordProcessorAction, set_mark_action, splice_text_action,
    split_block_action,
};

fn fresh_writer_document() -> AutoCommit {
    let mut doc = AutoCommit::new();
    word_processor::initialize_empty_document(&mut doc).unwrap();
    doc.commit();
    doc
}

fn fork(doc: &mut AutoCommit) -> (AutoCommit, AutoCommit) {
    let a = AutoCommit::load(&doc.save()).unwrap();
    let mut b = AutoCommit::load(&doc.save()).unwrap();
    b.set_actor(automerge::ActorId::random());
    (a, b)
}

fn apply(doc: &mut AutoCommit, action: &WordProcessorAction) {
    action.apply(doc).unwrap();
    doc.commit();
}

/// Merge both directions and require identical results.
fn merge_both(a: &mut AutoCommit, b: &mut AutoCommit) -> qdoc::DocSnapshot {
    let mut a_copy = a.clone();
    let mut b_copy = b.clone();
    a.merge(&mut b_copy).unwrap();
    b.merge(&mut a_copy).unwrap();
    let snap_a = snapshot(a);
    let snap_b = snapshot(b);
    assert_eq!(
        snap_a, snap_b,
        "replicas converge regardless of merge order"
    );
    snap_a
}

fn snapshot(doc: &AutoCommit) -> qdoc::DocSnapshot {
    let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
    qdoc::model::materialize(doc, &c)
}

fn blocks(doc: &AutoCommit) -> Vec<String> {
    let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
    qdoc::model::blocks_in_order(doc, &c)
}

fn only_text(snap: &qdoc::DocSnapshot) -> &str {
    assert_eq!(snap.blocks.len(), 1, "{snap:?}");
    &snap.blocks[0].text
}

fn splice(block: &str, at: usize, del: usize, ins: &str) -> WordProcessorAction {
    splice_text_action(RegionKind::Body, block, at, at + del, ins)
}

#[test]
fn concurrent_first_typing_in_a_fresh_paragraph_keeps_both_sides() {
    let mut base = fresh_writer_document();
    let block = blocks(&base)[0].clone();
    let (mut a, mut b) = fork(&mut base);
    apply(&mut a, &splice(&block, 0, 0, "alpha "));
    apply(&mut b, &splice(&block, 0, 0, "beta "));
    let snap = merge_both(&mut a, &mut b);
    let text = only_text(&snap);
    assert!(
        text.contains("alpha ") && text.contains("beta "),
        "both concurrent first edits survive, got {text:?}"
    );
    assert_eq!(text.chars().count(), "alpha beta ".chars().count());
}

#[test]
fn concurrent_typing_into_a_new_empty_paragraph_keeps_both_sides() {
    let mut base = fresh_writer_document();
    let first = blocks(&base)[0].clone();
    apply(&mut base, &splice(&first, 0, 0, "Intro"));
    // Enter at the end of a paragraph creates an empty paragraph; both sides
    // then start typing into it before seeing each other.
    apply(
        &mut base,
        &split_block_action(RegionKind::Body, &first, 5, "paragraph", None),
    );
    let second = blocks(&base)[1].clone();
    let (mut a, mut b) = fork(&mut base);
    apply(&mut a, &splice(&second, 0, 0, "from A"));
    apply(&mut b, &splice(&second, 0, 0, "from B"));
    let snap = merge_both(&mut a, &mut b);
    assert_eq!(snap.blocks[0].text, "Intro");
    let text = &snap.blocks[1].text;
    assert!(
        text.contains("from A") && text.contains("from B"),
        "got {text:?}"
    );
}

#[test]
fn concurrent_inserts_at_different_positions_merge_character_level() {
    let mut base = fresh_writer_document();
    let block = blocks(&base)[0].clone();
    apply(&mut base, &splice(&block, 0, 0, "Hello world"));
    let (mut a, mut b) = fork(&mut base);
    apply(&mut a, &splice(&block, 0, 0, ">> "));
    apply(&mut b, &splice(&block, 11, 0, "!"));
    apply(&mut b, &splice(&block, 5, 0, ","));
    let snap = merge_both(&mut a, &mut b);
    assert_eq!(only_text(&snap), ">> Hello, world!");
}

#[test]
fn concurrent_insert_and_delete_in_one_paragraph_preserve_both_intents() {
    let mut base = fresh_writer_document();
    let block = blocks(&base)[0].clone();
    apply(&mut base, &splice(&block, 0, 0, "Hello cruel world"));
    let (mut a, mut b) = fork(&mut base);
    // A deletes "cruel ", B inserts "big " before "world" and appends.
    apply(&mut a, &splice(&block, 6, 6, ""));
    apply(&mut b, &splice(&block, 12, 0, "big "));
    apply(&mut b, &splice(&block, 21, 0, "!"));
    let snap = merge_both(&mut a, &mut b);
    assert_eq!(only_text(&snap), "Hello big world!");
}

#[test]
fn concurrent_formatting_and_typing_in_one_paragraph_both_survive() {
    let mut base = fresh_writer_document();
    let block = blocks(&base)[0].clone();
    apply(&mut base, &splice(&block, 0, 0, "Hello world"));
    let (mut a, mut b) = fork(&mut base);
    apply(
        &mut a,
        &set_mark_action(
            RegionKind::Body,
            vec![Span {
                block: block.clone(),
                start: 6,
                end: 11,
            }],
            "bold",
            Some(MarkVal::Bool(true)),
            vec![],
        ),
    );
    apply(&mut b, &splice(&block, 0, 0, "Say: "));
    let snap = merge_both(&mut a, &mut b);
    let paragraph = &snap.blocks[0];
    assert_eq!(paragraph.text, "Say: Hello world");
    let bold: Vec<_> = paragraph
        .marks
        .iter()
        .filter(|mark| mark.name == "bold")
        .map(|mark| (mark.start, mark.end))
        .collect();
    assert_eq!(bold, vec![(11, 16)], "the mark follows the characters");
}

#[test]
fn concurrent_formatting_and_first_typing_in_a_fresh_paragraph_both_survive() {
    let mut base = fresh_writer_document();
    let block = blocks(&base)[0].clone();
    let (mut a, mut b) = fork(&mut base);
    apply(&mut a, &splice(&block, 0, 0, "bold"));
    apply(
        &mut a,
        &set_mark_action(
            RegionKind::Body,
            vec![Span {
                block: block.clone(),
                start: 0,
                end: 4,
            }],
            "bold",
            Some(MarkVal::Bool(true)),
            vec![],
        ),
    );
    apply(&mut b, &splice(&block, 0, 0, "plain"));
    let snap = merge_both(&mut a, &mut b);
    let paragraph = &snap.blocks[0];
    assert!(
        paragraph.text.contains("bold") && paragraph.text.contains("plain"),
        "got {:?}",
        paragraph.text
    );
    assert!(paragraph.marks.iter().any(|mark| mark.name == "bold"));
}

/// A document written by the old model has a block without a text object; two
/// replicas that each created one concurrently hold a conflicted `text` key.
fn legacy_conflicted_document() -> (AutoCommit, String) {
    let mut base = fresh_writer_document();
    let block = blocks(&base)[0].clone();
    let c = qdoc::model::open(&base, &automerge::ROOT).unwrap();
    let (_, block_obj) = base.get(&c.blocks, &block).unwrap().unwrap();
    // Emulate the pre-fix shape: the block has no text object.
    if base
        .get(&block_obj, qdoc::schema::BLOCK_TEXT)
        .unwrap()
        .is_some()
    {
        base.delete(&block_obj, qdoc::schema::BLOCK_TEXT).unwrap();
        base.commit();
    }
    let (mut a, mut b) = fork(&mut base);
    for (doc, text) in [(&mut a, "left "), (&mut b, "right ")] {
        let t = doc
            .put_object(&block_obj, qdoc::schema::BLOCK_TEXT, ObjType::Text)
            .unwrap();
        doc.splice_text(&t, 0, 0, text).unwrap();
        doc.commit();
    }
    let mut b_copy = b.clone();
    a.merge(&mut b_copy).unwrap();
    (a, block)
}

#[test]
fn legacy_conflicted_paragraph_opens_with_both_sides_and_upgrades_on_edit() {
    let (mut doc, block) = legacy_conflicted_document();
    let before = only_text(&snapshot(&doc)).to_string();
    assert!(
        before.contains("left ") && before.contains("right "),
        "the read path shows every conflicting paragraph value, got {before:?}"
    );
    let len = before.chars().count();
    apply(&mut doc, &splice(&block, len, 0, "end"));
    let after = only_text(&snapshot(&doc)).to_string();
    assert_eq!(
        after,
        format!("{before}end"),
        "offsets address the shown text"
    );
    let c = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let (_, block_obj) = doc.get(&c.blocks, &block).unwrap().unwrap();
    assert_eq!(
        doc.get_all(&block_obj, qdoc::schema::BLOCK_TEXT)
            .unwrap()
            .len(),
        1,
        "the first edit resolves the conflict into one text object"
    );
}

#[test]
fn legacy_scalar_paragraph_opens_and_upgrades_to_text_on_first_edit() {
    let mut doc = fresh_writer_document();
    let block = blocks(&doc)[0].clone();
    let c = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let (_, block_obj) = doc.get(&c.blocks, &block).unwrap().unwrap();
    doc.put(&block_obj, qdoc::schema::BLOCK_TEXT, "legacy words")
        .unwrap();
    doc.commit();
    assert_eq!(only_text(&snapshot(&doc)), "legacy words");
    let history = doc.get_changes(&[]).len();
    let (mut a, mut b) = fork(&mut doc);
    apply(&mut a, &splice(&block, 12, 0, " kept"));
    let snap = snapshot(&a);
    assert_eq!(only_text(&snap), "legacy words kept");
    assert!(matches!(
        a.get(&block_obj, qdoc::schema::BLOCK_TEXT).unwrap(),
        Some((automerge::Value::Object(ObjType::Text), _))
    ));
    assert!(
        a.get_changes(&[]).len() > history,
        "history is extended, never rewritten"
    );
    // Once upgraded, further concurrent edits merge character-level.
    let mut a_copy = a.clone();
    b.merge(&mut a_copy).unwrap();
    apply(&mut a, &splice(&block, 0, 0, "A:"));
    apply(&mut b, &splice(&block, 17, 0, " B"));
    let snap = merge_both(&mut a, &mut b);
    assert_eq!(only_text(&snap), "A:legacy words kept B");
}
