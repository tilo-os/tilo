#![allow(
    clippy::arithmetic_side_effects,
    reason = "test positions and counters"
)]

//! The rich-text resource read incrementally: after any edit, what a view
//! holds equals a fresh read of the same parts, and a keystroke reads only
//! its paragraph.

use super::*;
use crate::action::{RegionKind, Span};
use tilo_resource::Replica;

fn body_ids(state: &RichTextState) -> Vec<String> {
    state.body().all().map(str::to_owned).collect()
}

/// A replica whose document has `paragraphs` paragraphs.
fn document(paragraphs: usize) -> Replica<RichText> {
    let mut replica = Replica::<RichText>::new();
    for n in 0..paragraphs {
        let last = replica
            .state()
            .unwrap()
            .body()
            .all()
            .last()
            .unwrap()
            .to_owned();
        replica
            .apply_action(
                &RichTextAction::InsertParagraph {
                    after: last,
                    text: format!("Paragraph {n} of the document."),
                },
                "Add paragraph",
            )
            .unwrap();
    }
    replica
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap_or(0) % bound.max(1)
    }
}

fn random_edit(replica: &mut Replica<RichText>, rng: &mut Lcg, step: usize) {
    let state = replica.state().unwrap();
    let ids = body_ids(&state);
    let id = ids[rng.next(ids.len())].clone();
    let len = state.containers().clone();
    let text_len = model::block_text(replica.doc(), &len, &id)
        .unwrap_or_default()
        .chars()
        .count();
    let action = match rng.next(8) {
        0..=2 => RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: id,
            at: rng.next(text_len + 1),
            del: 0,
            ins: format!("x{step}"),
        },
        3 => RichTextAction::SplitBlock {
            region: RegionKind::Body,
            block: id,
            at: rng.next(text_len + 1),
            kind: "paragraph".into(),
            ordered: None,
            next: None,
        },
        4 => RichTextAction::SetParagraphStyle {
            region: RegionKind::Body,
            blocks: vec![id],
            kind: if step.is_multiple_of(2) {
                "heading"
            } else {
                "paragraph"
            }
            .into(),
            level: Some(i64::try_from(1 + step % 3).unwrap_or(1)),
            ordered: None,
        },
        5 if ids.len() > 2 => RichTextAction::RemoveBlocks { blocks: vec![id] },
        6 if text_len > 1 => RichTextAction::SetMark {
            region: RegionKind::Body,
            spans: vec![Span {
                block: id,
                start: 0,
                end: text_len.min(3),
            }],
            name: "strong".into(),
            value: Some(action::MarkVal::Bool(true)),
            also_clear: vec![],
        },
        _ => RichTextAction::InsertParagraph {
            after: id,
            text: format!("inserted {step}"),
        },
    };
    replica.apply_action(&action, "Edit").unwrap();
}

#[test]
fn a_view_of_a_window_equals_a_fresh_read_after_every_edit() {
    let mut rng = Lcg(5);
    let mut replica = document(40);
    let demand = Demand::range(10..25);
    replica.demand(demand.clone()).unwrap();
    for step in 0..250 {
        random_edit(&mut replica, &mut rng, step);
        let mut fresh = replica.state().unwrap();
        RichText::load(&mut fresh, replica.doc(), &demand).unwrap();
        let live = replica.live().unwrap();
        assert_eq!(live, &fresh, "after step {step}");
        assert_eq!(
            live.blocks(10..25).count(),
            15.min(live.body().len().saturating_sub(10))
        );
    }
}

#[test]
fn a_keystroke_reads_its_paragraph_and_not_the_document() {
    let mut replica = document(400);
    let state = replica.state().unwrap();
    let target = state.body().id_at(200).unwrap().to_owned();
    replica.demand(Demand::range(190..210)).unwrap();

    replica
        .apply_action(
            &RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: target.clone(),
                at: 0,
                del: 0,
                ins: "k".into(),
            },
            "Type",
        )
        .unwrap();
    let scans = model::ordered_keys_scan_count();
    let body_before = Rc::clone(replica.live().unwrap().body());
    let live = replica.live().unwrap();
    assert_eq!(model::ordered_keys_scan_count(), scans, "no container scan");
    assert!(
        Rc::ptr_eq(&body_before, live.body()),
        "the paragraph order is untouched"
    );
    assert!(live.block(&target).unwrap().text.starts_with('k'));

    // Splitting a paragraph moves one entry of the order, still without a scan.
    replica
        .apply_action(
            &RichTextAction::SplitBlock {
                region: RegionKind::Body,
                block: target,
                at: 1,
                kind: "paragraph".into(),
                ordered: None,
                next: None,
            },
            "Enter",
        )
        .unwrap();
    let scans = model::ordered_keys_scan_count();
    let live = body_ids(replica.live().unwrap());
    assert_eq!(model::ordered_keys_scan_count(), scans);
    assert_eq!(live.len(), 402);
    assert_eq!(live, body_ids(&replica.state().unwrap()));
}

#[test]
fn replayed_actions_mint_the_same_blocks() {
    let mut origin = document(3);
    let mut peer = origin.fork();
    let first = origin.state().unwrap().body().id_at(1).unwrap().to_owned();
    origin
        .apply_action(
            &RichTextAction::SplitBlock {
                region: RegionKind::Body,
                block: first,
                at: 3,
                kind: "paragraph".into(),
                ordered: None,
                next: None,
            },
            "Enter",
        )
        .unwrap();
    let envelope = origin.log().last().unwrap().clone();
    peer.replay(&envelope).unwrap();
    assert_eq!(
        body_ids(&origin.state().unwrap()),
        body_ids(&peer.state().unwrap())
    );
}

#[test]
fn the_outline_follows_headings() {
    let mut replica = document(6);
    let id = replica.state().unwrap().body().id_at(3).unwrap().to_owned();
    replica.live().unwrap();
    replica
        .apply_action(
            &RichTextAction::SetParagraphStyle {
                region: RegionKind::Body,
                blocks: vec![id.clone()],
                kind: "heading".into(),
                level: Some(2),
                ordered: None,
            },
            "Heading",
        )
        .unwrap();
    let outline = replica.live().unwrap().headings();
    assert_eq!(outline.len(), 1);
    assert_eq!(outline[0].id, id);
    assert_eq!(outline[0].level, 2);
    replica
        .apply_action(
            &RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: id,
                at: 0,
                del: 0,
                ins: "Chapter: ".into(),
            },
            "Type",
        )
        .unwrap();
    let outline = replica.live().unwrap().headings();
    assert!(outline[0].text.starts_with("Chapter: "));
    assert_eq!(outline, replica.state().unwrap().headings());
}
