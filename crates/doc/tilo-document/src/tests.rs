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
            .apply_action(&RichTextAction::InsertParagraph {
                after: last,
                text: format!("Paragraph {n} of the document."),
            })
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
    replica.apply_action(&action).unwrap();
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
        .apply_action(&RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: target.clone(),
            at: 0,
            del: 0,
            ins: "k".into(),
        })
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
        .apply_action(&RichTextAction::SplitBlock {
            region: RegionKind::Body,
            block: target,
            at: 1,
            kind: "paragraph".into(),
            ordered: None,
            next: None,
        })
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
        .apply_action(&RichTextAction::SplitBlock {
            region: RegionKind::Body,
            block: first,
            at: 3,
            kind: "paragraph".into(),
            ordered: None,
            next: None,
        })
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
        .apply_action(&RichTextAction::SetParagraphStyle {
            region: RegionKind::Body,
            blocks: vec![id.clone()],
            kind: "heading".into(),
            level: Some(2),
            ordered: None,
        })
        .unwrap();
    let outline = replica.live().unwrap().headings();
    assert_eq!(outline.len(), 1);
    assert_eq!(outline[0].id, id);
    assert_eq!(outline[0].level, 2);
    replica
        .apply_action(&RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: id,
            at: 0,
            del: 0,
            ins: "Chapter: ".into(),
        })
        .unwrap();
    let outline = replica.live().unwrap().headings();
    assert!(outline[0].text.starts_with("Chapter: "));
    assert_eq!(outline, replica.state().unwrap().headings());
}

/// Rich text stored as a field of another document: edited, read and
/// refreshed under its own map, never touching the document's root or a
/// sibling field.
#[test]
fn rich_text_under_a_field_is_edited_and_refreshed_in_place() {
    use automerge::transaction::Transactable;
    let mut doc = automerge::AutoCommit::new();
    let messages = doc
        .put_object(automerge::ROOT, "messages", automerge::ObjType::Map)
        .unwrap();
    let first = doc
        .put_object(&messages, "m1", automerge::ObjType::Map)
        .unwrap();
    let second = doc
        .put_object(&messages, "m2", automerge::ObjType::Map)
        .unwrap();
    RichText::initialize_under(&mut doc, &first).unwrap();
    RichText::initialize_under(&mut doc, &second).unwrap();
    doc.commit();
    assert!(
        RichText::read(&doc).is_err(),
        "the root holds no rich text of its own"
    );

    let mut state = RichText::read_under(&doc, &first).unwrap();
    let everything = tilo_resource::Demand::range(0..usize::MAX);
    RichText::load(&mut state, &doc, &everything).unwrap();
    let block = state.body().id_at(0).unwrap().to_owned();

    let mut tracker = tilo_resource::Tracker::new();
    tracker.start(&mut doc);
    RichTextAction::SpliceText {
        region: RegionKind::Body,
        block: block.clone(),
        at: 0,
        del: 0,
        ins: "hello".into(),
    }
    .apply_under(&mut doc, &first)
    .unwrap();
    doc.commit();
    let delta = tracker.delta_located(&mut doc, &|obj| RichText::locate(&state, obj));
    RichText::refresh_under(&mut state, &doc, &delta).unwrap();
    RichText::load(&mut state, &doc, &everything).unwrap();
    assert_eq!(state.block(&block).unwrap().text, "hello");
    RichTextAction::SplitBlock {
        region: RegionKind::Body,
        block,
        at: 5,
        kind: "paragraph".into(),
        ordered: None,
        next: None,
    }
    .apply_under(&mut doc, &first)
    .unwrap();
    doc.commit();
    let delta = tracker.delta_located(&mut doc, &|obj| RichText::locate(&state, obj));
    RichText::refresh_under(&mut state, &doc, &delta).unwrap();
    RichText::load(&mut state, &doc, &everything).unwrap();
    assert_eq!(
        RichText::plain_text_under(&doc, &first).unwrap(),
        "hello\n",
        "a line per paragraph"
    );
    assert!(RichText::plain_text_under(&doc, &messages).is_err());

    let mut fresh = RichText::read_under(&doc, &first).unwrap();
    RichText::load(&mut fresh, &doc, &everything).unwrap();
    assert_eq!(state, fresh, "the refreshed field equals a fresh read");

    let mut other = RichText::read_under(&doc, &second).unwrap();
    RichText::load(&mut other, &doc, &everything).unwrap();
    let other_block = other.body().id_at(0).unwrap().to_owned();
    assert_eq!(
        other.block(&other_block).unwrap().text,
        "",
        "the sibling is untouched"
    );
}

/// The cells of every table in the body, in order: `(table, row, col, id, text)`.
fn table_cells(replica: &Replica<RichText>) -> Vec<(String, i64, i64, String, String)> {
    let c = replica.state().unwrap().containers().clone();
    model::materialize(replica.doc(), &c)
        .blocks
        .into_iter()
        .filter(|block| block.kind == "cell")
        .map(|block| {
            let int = |key: &str| match block.attrs.get(key) {
                Some(AttrVal::Int(value)) => *value,
                other => panic!("{key} is {other:?}"),
            };
            let table = match block.attrs.get("table") {
                Some(AttrVal::Str(table)) => table.clone(),
                other => panic!("table is {other:?}"),
            };
            (table, int("row"), int("col"), block.id, block.text)
        })
        .collect()
}

fn team() -> Vec<Vec<String>> {
    [
        ["Name", "Role", "Location"],
        ["Priya Natarajan", "Product lead", "Berlin"],
        ["Tom Becker", "Engineering", "Hamburg"],
    ]
    .iter()
    .map(|row| row.iter().map(|&text| text.to_owned()).collect())
    .collect()
}

#[test]
fn a_filled_table_is_laid_out_row_by_row_with_its_header_first() {
    let mut replica = document(2);
    let ids = body_ids(&replica.state().unwrap());
    let action = action::insert_filled_table_action(&ids[1], team());
    replica.apply_action(&action).unwrap();

    let cells = table_cells(&replica);
    assert_eq!(cells.len(), 9);
    let table = &cells[0].0;
    assert!(cells.iter().all(|cell| &cell.0 == table), "one table");
    let laid_out: Vec<(i64, i64, &str)> = cells
        .iter()
        .map(|(_, row, col, _, text)| (*row, *col, text.as_str()))
        .collect();
    assert_eq!(
        laid_out,
        [
            (0, 0, "Name"),
            (0, 1, "Role"),
            (0, 2, "Location"),
            (1, 0, "Priya Natarajan"),
            (1, 1, "Product lead"),
            (1, 2, "Berlin"),
            (2, 0, "Tom Becker"),
            (2, 1, "Engineering"),
            (2, 2, "Hamburg"),
        ]
    );
    // In place: right after `after`, before the paragraph that followed it.
    let body = body_ids(&replica.state().unwrap());
    let cell_ids: Vec<&str> = cells.iter().map(|cell| cell.3.as_str()).collect();
    assert_eq!(body[..2], ids[..2]);
    assert_eq!(body[2..11], cell_ids[..]);
    assert_eq!(body[11..], ids[2..]);
    // The resource's state lists the cells as the document's objects.
    assert_eq!(replica.state().unwrap().featured("cell").len(), 9);
}

#[test]
fn replicas_applying_a_filled_table_mint_the_same_table() {
    let mut origin = document(2);
    let mut peer = origin.fork();
    let after = origin.state().unwrap().body().id_at(1).unwrap().to_owned();
    origin
        .apply_action(&action::insert_filled_table_action(&after, team()))
        .unwrap();
    let envelope = origin.log().last().unwrap().clone();
    peer.replay(&envelope).unwrap();
    assert_eq!(table_cells(&origin), table_cells(&peer));
    assert_eq!(
        body_ids(&origin.state().unwrap()),
        body_ids(&peer.state().unwrap())
    );
    assert_eq!(origin.state().unwrap(), peer.state().unwrap());
}

#[test]
fn a_filled_table_and_the_edits_after_it_are_one_group() {
    use tilo_resource::Invoke;
    let mut origin = document(3);
    let mut peer = origin.fork();
    let ids = body_ids(&origin.state().unwrap());
    // Replace the paragraphs after the first with a table, in one change.
    let group = Invoke::group([
        Invoke::<RichText>::apply(&action::insert_filled_table_action(&ids[0], team())).unwrap(),
        Invoke::<RichText>::apply(&RichTextAction::RemoveBlocks {
            blocks: ids[1..].to_vec(),
        })
        .unwrap(),
    ]);
    let before = origin.log().len();
    origin.apply(group).unwrap();
    assert_eq!(origin.log().len(), before + 1, "one change");
    let body = body_ids(&origin.state().unwrap());
    assert_eq!(body.len(), 10);
    assert_eq!(body[0], ids[0]);
    assert_eq!(table_cells(&origin)[4].4, "Product lead");

    let envelope = origin.log().last().unwrap().clone();
    peer.replay(&envelope).unwrap();
    assert_eq!(table_cells(&origin), table_cells(&peer));
    assert_eq!(body, body_ids(&peer.state().unwrap()));
}

#[test]
fn an_empty_table_still_replays_as_it_always_has() {
    // A recorded payload decodes to the same action and re-encodes to the
    // same bytes, so the ids it mints on replay are the ones it minted then.
    let recorded = r#"{"InsertTable":{"after":"b1","rows":2,"cols":3}}"#;
    let action: RichTextAction = serde_json::from_str(recorded).unwrap();
    assert_eq!(action, action::insert_table_action("b1", 2, 3));
    assert_eq!(serde_json::to_string(&action).unwrap(), recorded);

    let mut replica = document(1);
    let after = replica.state().unwrap().body().id_at(1).unwrap().to_owned();
    replica
        .apply_action(&action::insert_table_action(&after, 2, 3))
        .unwrap();
    let cells = table_cells(&replica);
    assert_eq!(cells.len(), 6);
    assert!(cells.iter().all(|cell| cell.4.is_empty()));
}

#[test]
fn a_filled_table_outside_the_limits_is_refused_before_it_writes() {
    let mut replica = document(1);
    let after = replica.state().unwrap().body().id_at(1).unwrap().to_owned();
    let row = |cols: usize, text: &str| vec![text.to_owned(); cols];
    let hostile = [
        // Nothing to lay out.
        Vec::new(),
        vec![Vec::new()],
        // Ragged rows.
        vec![row(3, "a"), row(2, "b")],
        // Past the table bounds.
        vec![row(action::MAX_TABLE_DIMENSION + 1, "")],
        vec![row(1, ""); action::MAX_TABLE_DIMENSION + 1],
        // More text than one action may carry, spread over the cells.
        vec![row(2, &"x".repeat(action::MAX_ACTION_TEXT_CHARS / 2 + 1))],
    ];
    let heads = replica.heads();
    for cells in hostile {
        let shape: Vec<usize> = cells.iter().map(Vec::len).collect();
        let refused = replica.apply_action(&action::insert_filled_table_action(&after, cells));
        assert!(refused.is_err(), "{shape:?} was accepted");
    }
    // A stale anchor.
    assert!(
        replica
            .apply_action(&action::insert_filled_table_action("gone", team()))
            .is_err()
    );
    assert_eq!(replica.heads(), heads, "nothing was written");
    assert!(table_cells(&replica).is_empty());

    // The largest table the bounds allow is accepted.
    let side = action::MAX_TABLE_DIMENSION;
    let cells = vec![row(side, "c"); side];
    replica
        .apply_action(&action::insert_filled_table_action(&after, cells))
        .unwrap();
    assert_eq!(table_cells(&replica).len(), side * side);
}

#[test]
fn paragraphs_added_at_once_into_one_gap_leave_room_between_them() {
    let mut local = document(2);
    let ids = body_ids(&local.state().unwrap());
    let mut peer = local.fork();
    for (replica, text) in [(&mut local, "Local"), (&mut peer, "Peer")] {
        replica
            .apply_action(&RichTextAction::InsertParagraph {
                after: ids[0].clone(),
                text: text.into(),
            })
            .unwrap();
    }
    local.sync_with(&mut peer).unwrap();
    let merged = body_ids(&local.state().unwrap());
    assert_eq!(merged, body_ids(&peer.state().unwrap()));
    assert_eq!(merged.len(), 5);
    local
        .apply_action(&RichTextAction::InsertParagraph {
            after: merged[1].clone(),
            text: "Between".into(),
        })
        .unwrap();
    let after = body_ids(&local.state().unwrap());
    assert_eq!(after.len(), 6);
    assert_eq!(after[1], merged[1]);
    assert_eq!(after[3], merged[2], "the new paragraph is between the two");
    let containers = local.state().unwrap().containers().clone();
    assert_eq!(
        model::block_text(local.doc(), &containers, &after[2]).as_deref(),
        Some("Between")
    );
}

#[test]
fn a_field_paragraph_forgotten_leaves_no_objects_behind() {
    use automerge::transaction::Transactable;
    let mut doc = Doc::new();
    let field = doc
        .put_object(automerge::ROOT, "field", automerge::ObjType::Map)
        .unwrap();
    RichText::initialize_under(&mut doc, &field).unwrap();
    // The first fills the empty paragraph; the second adds one.
    for text in ["first", "second"] {
        RichTextAction::AppendParagraph { text: text.into() }
            .apply_under(&mut doc, &field)
            .unwrap();
    }
    let mut state = RichText::read_under(&doc, &field).unwrap();
    RichText::load(&mut state, &doc, &Demand::range(0..usize::MAX)).unwrap();
    let ids = body_ids(&state);
    assert_eq!(ids.len(), 2);
    let before = state.objects.len();
    assert!(before >= 2 * ids.len(), "each paragraph's map and text");
    state.forget_paragraph(&ids[0]);
    assert!(state.objects.len() < before);
    state.forget_paragraph(&ids[1]);
    assert!(state.objects.is_empty(), "{:?}", state.objects);
}

#[test]
fn a_reply_is_written_as_its_paragraphs_marks_and_attributes() {
    use crate::{ClipAttr, ClipBlock};
    let mut replica = document(1);
    let block = body_ids(&replica.state().unwrap()).pop().unwrap();
    replica
        .apply_action(&RichTextAction::AddComment {
            block,
            start: 0,
            end: 4,
            author: "Ada".into(),
            date: String::new(),
            text: "Why?".into(),
        })
        .unwrap();
    let comment = replica.state().unwrap().comments()[0].id.clone();
    let body = vec![
        ClipBlock {
            kind: "paragraph".into(),
            text: "plain bold".into(),
            marks: vec![tilo_doc::document::MarkSpan {
                start: 6,
                end: 10,
                name: "strong".into(),
                value: tilo_doc::document::MarkValue::Bool(true),
            }],
            level: None,
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::new(),
        },
        ClipBlock {
            kind: "list_item".into(),
            text: "done".into(),
            marks: Vec::new(),
            level: Some(0),
            ordered: Some(false),
            align: None,
            attrs: [("checked".to_owned(), ClipAttr::Bool(true))].into(),
        },
    ];
    replica
        .apply_action(&RichTextAction::ReplyToComment {
            comment: comment.clone(),
            author: "Grace".into(),
            date: String::new(),
            text: "plain bold\ndone".into(),
            body,
        })
        .unwrap();
    let state = replica.state().unwrap();
    let thread = state
        .comments()
        .iter()
        .find(|entry| entry.id == comment)
        .unwrap();
    let reply = &thread.replies[0];
    assert_eq!(reply.author, "Grace");
    let texts: Vec<&str> = reply
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect();
    assert_eq!(texts, ["plain bold", "done"]);
    assert!(
        reply.blocks[0]
            .marks
            .iter()
            .any(|mark| mark.name == "strong" && (mark.start, mark.end) == (6, 10))
    );
    assert_eq!(reply.blocks[1].kind, "list_item");
    assert_eq!(
        reply.blocks[1].attrs.get("checked"),
        Some(&tilo_doc::AttrVal::Bool(true))
    );
}

/// Apply `action` as a window does for `writer`: the envelope pins who
/// invoked it.
fn apply_as(
    replica: &mut Replica<RichText>,
    action: &RichTextAction,
    writer: &str,
) -> tilo_actions::ActionEnvelope {
    let encoded = tilo_actions::EncodedAction::encode(RICH_TEXT_ACTION_TYPE, action).unwrap();
    let context =
        tilo_actions::ActionContext::for_resource(RichText::KIND, RichText::KIND, RichText::LABEL)
            .with_invoker(tilo_actions::ActionInvoker::actor(writer));
    tilo_actions::apply_local_with_context(
        replica.doc_mut(),
        action,
        encoded,
        Some(b"writer-device"),
        context,
    )
    .unwrap()
}

/// Every author run in the body: the paragraph's position, the run, whose.
fn author_runs(replica: &Replica<RichText>) -> Vec<(usize, usize, usize, String)> {
    let doc = replica.doc();
    let c = model::open(doc, &automerge::ROOT).unwrap();
    model::materialize(doc, &c)
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(index, block)| {
            tilo_doc::author_runs(&block.marks)
                .into_iter()
                .map(move |(start, end, author)| (index, start, end, author))
        })
        .collect()
}

#[test]
fn text_typed_in_carries_its_invokers_name_from_the_one_change_that_wrote_it() {
    type Case = (fn(&[String]) -> RichTextAction, (usize, usize, usize));
    let paragraph = "Paragraph 0 of the document.".chars().count();
    let cases: [Case; 5] = [
        (
            |ids| RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: ids[1].clone(),
                at: 10,
                del: 0,
                ins: "zero ".into(),
            },
            (1, 10, 15),
        ),
        (
            |ids| RichTextAction::ReplaceSelection {
                region: RegionKind::Body,
                start_block: ids[1].clone(),
                start: 0,
                end_block: ids[1].clone(),
                end: 9,
                ins: "Section".into(),
            },
            (1, 0, 7),
        ),
        (
            |ids| RichTextAction::InsertParagraph {
                after: ids[1].clone(),
                text: "Next".into(),
            },
            (2, 0, 4),
        ),
        (
            |_| RichTextAction::AppendParagraph { text: "End".into() },
            (2, 0, 3),
        ),
        (
            |_| RichTextAction::ContinueParagraph {
                text: " More.".into(),
            },
            (1, paragraph, paragraph + 6),
        ),
    ];
    for (action, (index, start, end)) in cases {
        let mut origin = document(1);
        let mut peer = origin.fork();
        let action = action(&body_ids(&origin.state().unwrap()));
        let changes = origin.doc_mut().get_changes(&[]).len();
        let envelope = apply_as(&mut origin, &action, "ada");
        assert_eq!(
            origin.doc_mut().get_changes(&[]).len(),
            changes + 1,
            "{action:?} and its authorship are one change"
        );
        assert_eq!(
            author_runs(&origin),
            [(index, start, end, "ada".to_owned())],
            "{action:?} marks exactly what it typed"
        );

        // The peer replays the envelope into the very same change.
        peer.replay(&envelope).unwrap();
        assert_eq!(
            origin.heads(),
            peer.heads(),
            "{action:?} replays identically"
        );
        assert_eq!(author_runs(&peer), author_runs(&origin));
    }
}

#[test]
fn text_nobody_invoked_and_text_taken_away_stay_unattributed() {
    let mut replica = document(1);
    let ids = body_ids(&replica.state().unwrap());
    replica
        .apply_action(&RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: ids[1].clone(),
            at: 0,
            del: 0,
            ins: "Unsigned ".into(),
        })
        .unwrap();
    assert!(author_runs(&replica).is_empty());

    apply_as(
        &mut replica,
        &RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: ids[1].clone(),
            at: 0,
            del: 9,
            ins: String::new(),
        },
        "ada",
    );
    assert!(author_runs(&replica).is_empty());
}

#[test]
fn replace_matches_refuses_invalid_batches_before_any_mutation() {
    let mut replica = document(1);
    let id = replica.state().unwrap().body().id_at(1).unwrap().to_owned();
    let span = |start, end| Span {
        block: id.clone(),
        start,
        end,
    };
    let heads = replica.heads();
    for (spans, ins) in [
        (vec![], "x".into()),
        (vec![span(0, 1), span(2, usize::MAX)], "x".into()),
        (
            vec![
                span(0, 1),
                Span {
                    block: "gone".into(),
                    start: 0,
                    end: 1,
                },
            ],
            "x".into(),
        ),
        (vec![span(0, 2), span(1, 3)], "x".into()),
        (vec![span(0, 1), span(0, 1)], "x".into()),
        (vec![span(0, 1), span(2, 2)], "x".into()),
        (vec![span(0, 1), span(4, 2)], "x".into()),
        (vec![span(0, 1); 10_001], String::new()),
        (
            vec![span(0, 1), span(2, 3)],
            "x".repeat(action::MAX_ACTION_TEXT_CHARS / 2 + 1),
        ),
    ] {
        assert!(
            replica
                .apply_action(&RichTextAction::ReplaceMatches {
                    region: RegionKind::Body,
                    spans,
                    ins
                })
                .is_err()
        );
        assert_eq!(replica.heads(), heads, "invalid batch wrote a partial edit");
    }
}

#[test]
fn replace_matches_uses_pre_action_unicode_offsets_and_one_replay_envelope() {
    let mut replica = document(0);
    let id = replica.state().unwrap().body().id_at(0).unwrap().to_owned();
    replica
        .apply_action(&RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: id.clone(),
            at: 0,
            del: 0,
            ins: "😀alpha alpha 日".into(),
        })
        .unwrap();
    let mut peer = replica.fork();
    let logs = replica.log().len();
    replica
        .apply_action(&RichTextAction::ReplaceMatches {
            region: RegionKind::Body,
            spans: vec![
                Span {
                    block: id.clone(),
                    start: 7,
                    end: 12,
                },
                Span {
                    block: id.clone(),
                    start: 1,
                    end: 6,
                },
            ],
            ins: "β😀".into(),
        })
        .unwrap();
    assert_eq!(replica.log().len(), logs + 1);
    assert_eq!(
        model::block_text(replica.doc(), replica.state().unwrap().containers(), &id).unwrap(),
        "😀β😀 β😀 日"
    );
    peer.replay(replica.log().last().unwrap()).unwrap();
    assert_eq!(
        model::block_text(peer.doc(), peer.state().unwrap().containers(), &id).unwrap(),
        model::block_text(replica.doc(), replica.state().unwrap().containers(), &id).unwrap()
    );
    assert_eq!(body_ids(&peer.state().unwrap()), vec![id]);
}

/// A new document starts the same whoever creates it: its first paragraph's
/// id names no device.
#[test]
fn a_new_document_starts_the_same_on_every_device() {
    let body = |actor: &str| {
        let mut doc =
            automerge::AutoCommit::new().with_actor(automerge::ActorId::from(actor.as_bytes()));
        RichText::initialize(&mut doc).unwrap();
        body_ids(&RichText::read(&doc).unwrap())
    };
    assert_eq!(body("device-a"), body("device-b"));
    assert_eq!(body("device-a").len(), 1);
}

/// An imported file is remembered by its content address, and a write of
/// the file records what it then holds, as content from outside.
#[test]
fn the_document_remembers_the_file_it_was_read_from_and_written_to() {
    use tilo_actions::Action;
    let mut doc = automerge::AutoCommit::new();
    RichText::initialize(&mut doc).unwrap();
    assert_eq!(RichText::read(&doc).unwrap().file_digest(), None);
    let bytes = b"# Report\n";
    crate::action::RichTextAction::ReplaceFormatted {
        format: crate::action::ImportFormat::Markdown,
        bytes: bytes.to_vec(),
    }
    .apply(&mut doc)
    .unwrap();
    let digest = tilo_content::hash_bytes(bytes);
    assert_eq!(
        RichText::read(&doc).unwrap().file_digest(),
        Some(digest.as_str())
    );
    let written = crate::action::RichTextAction::NoteFileWritten {
        digest: tilo_content::hash_bytes(b"saved"),
    };
    assert_eq!(written.change_kind(), tilo_actions::ChangeKind::External);
    written.apply(&mut doc).unwrap();
    assert_eq!(
        RichText::read(&doc).unwrap().file_digest(),
        Some(tilo_content::hash_bytes(b"saved").as_str())
    );
}

/// The body's blocks as `(id, kind, alt)`.
fn block_alts(replica: &Replica<RichText>) -> Vec<(String, String, Option<String>)> {
    let c = replica.state().unwrap().containers().clone();
    model::materialize(replica.doc(), &c)
        .blocks
        .into_iter()
        .map(|block| {
            let alt = match block.attrs.get("alt") {
                Some(tilo_doc::AttrVal::Str(alt)) => Some(alt.clone()),
                None => None,
                other => panic!("alt is {other:?}"),
            };
            (block.id, block.kind, alt)
        })
        .collect()
}

#[test]
fn an_image_description_is_set_and_cleared_only_on_an_image() {
    let mut replica = document(1);
    let paragraph = replica.state().unwrap().body().id_at(1).unwrap().to_owned();
    replica
        .apply_action(&RichTextAction::InsertImage {
            block: paragraph.clone(),
            at: 0,
            content: "ab".repeat(32),
            mime: "image/jpeg".into(),
            len: 1_024,
            width: Some(640),
            height: Some(480),
        })
        .unwrap();
    let image = block_alts(&replica)
        .into_iter()
        .find(|(_, kind, _)| kind == "image")
        .map(|(id, _, _)| id)
        .expect("the image was inserted");
    let set = |blocks: Vec<String>, alt: &str| RichTextAction::SetBlockAttr {
        region: RegionKind::Body,
        blocks,
        key: "alt".into(),
        value: action::AttrVal::Str(alt.into()),
    };

    let heads = replica.heads();
    for (blocks, alt) in [
        // A paragraph has no description to set.
        (vec![paragraph.clone()], "A desk light"),
        // Nor does a mixed selection, even with an image in it.
        (vec![image.clone(), paragraph], "A desk light"),
        // Blank, multi-line and oversized descriptions.
        (vec![image.clone()], "   "),
        (vec![image.clone()], "A desk\nlight"),
        (
            vec![image.clone()],
            &"x".repeat(action::MAX_IMAGE_ALT_CHARS + 1),
        ),
        // A stale image.
        (vec!["gone".to_owned()], "A desk light"),
    ] {
        assert!(
            replica.apply_action(&set(blocks, alt)).is_err(),
            "{alt:?} was accepted"
        );
    }
    assert_eq!(replica.heads(), heads, "nothing was written");

    let alt = "Chalk-white articulated desk light on a desk, with a deep-teal campaign panel to the right.";
    replica
        .apply_action(&set(vec![image.clone()], alt))
        .unwrap();
    let described = block_alts(&replica);
    assert!(described.contains(&(image.clone(), "image".into(), Some(alt.to_owned()))));
    assert!(
        described
            .iter()
            .filter(|(id, _, _)| *id != image)
            .all(|(_, _, alt)| alt.is_none()),
        "only the image is described"
    );

    replica
        .apply_action(&RichTextAction::ClearBlockAttr {
            region: RegionKind::Body,
            blocks: vec![image],
            key: "alt".into(),
        })
        .unwrap();
    assert!(block_alts(&replica).iter().all(|(_, _, alt)| alt.is_none()));
}
