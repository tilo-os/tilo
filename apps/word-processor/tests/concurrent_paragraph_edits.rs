//! Two people editing the *same paragraph* concurrently must both keep their
//! characters: a paragraph is a character-level CRDT, never a last-writer-wins
//! register.
//!
//! Fault testing found the regression in the product (offline B typing into
//! the first paragraph of a fresh Writer document while A typed into the same
//! paragraph): after the merge only one side's text survived. The cause was
//! in the tilo-doc model, not the renderer: a new block carried no `text` object,
//! and the first splice on each replica created one lazily with
//! `put_object(block, "text", Text)`. Two concurrent creations are a map-key
//! conflict, so Automerge exposes only one winner and the loser's whole
//! paragraph disappears. These tests drive the canonical Writer actions (the
//! path typing, Ctrl+K and agents share) on two replicas and merge both ways.

use automerge::AutoCommit;
use tilo_automerge::ApplyIncoming as _;
use word_processor::actions::{
    MarkVal, RegionKind, RichTextAction, Span, set_mark_action, splice_text_action,
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

fn apply(doc: &mut AutoCommit, action: &RichTextAction) {
    word_processor::model::apply(doc, action).unwrap();
    doc.commit();
}

/// Merge both directions and require identical results.
fn merge_both(a: &mut AutoCommit, b: &mut AutoCommit) -> tilo_doc::DocSnapshot {
    let mut a_copy = a.clone();
    let mut b_copy = b.clone();
    a.merge_incoming(&mut b_copy).unwrap();
    b.merge_incoming(&mut a_copy).unwrap();
    let snap_a = snapshot(a);
    let snap_b = snapshot(b);
    assert_eq!(
        snap_a, snap_b,
        "replicas converge regardless of merge order"
    );
    snap_a
}

fn snapshot(doc: &AutoCommit) -> tilo_doc::DocSnapshot {
    let c = word_processor::model::open(doc).unwrap();
    tilo_doc::model::materialize(doc, &c)
}

fn blocks(doc: &AutoCommit) -> Vec<String> {
    let c = word_processor::model::open(doc).unwrap();
    tilo_doc::model::blocks_in_order(doc, &c)
}

fn only_text(snap: &tilo_doc::DocSnapshot) -> &str {
    assert_eq!(snap.blocks.len(), 1, "{snap:?}");
    &snap.blocks[0].text
}

fn splice(block: &str, at: usize, del: usize, ins: &str) -> RichTextAction {
    splice_text_action(RegionKind::Body, block, at, at.saturating_add(del), ins)
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
