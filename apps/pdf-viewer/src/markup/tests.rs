//! The record through its own door: field edits and verbs on replicas, how
//! concurrent edits merge, what is published, and that imported bytes stay
//! out of it.

use std::collections::{BTreeSet, HashSet};
use std::io::Read;
use std::sync::Mutex;

use tilo_actions::OperationKind;
use tilo_content::{
    ContentBackend, ContentError, ContentReader, ContentState, ContentStore, MemoryBackend, Stored,
};
use ui::framework::LiveRow;
use ui::tilo_resource::{Id, Replica, Resource as _};

use super::*;
use crate::actions::{
    PdfResourceCtx, SEARCH_RESULT_RESOURCE_LIMIT, add_annotation_on, add_comment_action,
    add_reply_action_with_id, add_signature_on, delete_annotation_action, delete_comment_action,
    delete_signature_action, import_pages_action, live_row, live_rows, move_page_plan_action,
    outline_resource_id, search_result_token, set_annotation_color_action,
    set_annotation_position_user_action, set_comment_resolved_action, set_comment_text_action,
};
use crate::model::{
    AnnoKind, Annotation, Comment, GeomSpace, PtRect, Reply, Signature, SignatureInk, Swatch,
};

/// A replica of a record whose plan lays out `pages` source pages.
fn planned_replica(pages: usize) -> Replica<PdfMarkup> {
    Replica::load(&PdfMarkup::planned(pages)).expect("the record loads")
}

fn markup(replica: &Replica<PdfMarkup>) -> PdfMarkup {
    replica.state().expect("the record reads")
}

fn anno(page: usize, kind: AnnoKind, color: Swatch, author: &str) -> Annotation {
    Annotation {
        id: Annotation::new_id(),
        page,
        kind,
        color,
        author: author.into(),
        chars: None,
        created_ms: 0,
        space: GeomSpace::Page,
    }
}

fn hl(page: usize) -> Annotation {
    anno(
        page,
        AnnoKind::highlight(PtRect::new(10.0, 20.0, 100.0, 14.0)),
        Swatch::Yellow,
        "Alice",
    )
}

fn signature(id: &str, page: usize, rect: PtRect, signer: &str, ink: SignatureInk) -> Signature {
    Signature {
        id: id.into(),
        page,
        rect,
        signer: signer.into(),
        date: "2026-06-30".into(),
        ink,
        space: GeomSpace::Page,
    }
}

/// The published resource `id` within `root`, wherever it nests.
fn find<'a>(root: &'a tilo_actions::Resource, id: &str) -> Option<&'a tilo_actions::Resource> {
    if root.id.as_str() == id {
        return Some(root);
    }
    root.children.iter().find_map(|child| find(child, id))
}

fn item(field: &str, id: &str) -> String {
    ui::tilo_resource::items::item_resource_id::<PdfMarkup>(field, &Id::from_raw(id))
}

fn action_ids(resource: &tilo_actions::Resource) -> Vec<&str> {
    resource.actions.iter().map(|a| a.id.as_str()).collect()
}

#[test]
fn a_comment_written_rich_keeps_its_marks_and_plain_text() {
    let mut replica = planned_replica(1);
    replica
        .apply(add_comment_action("c", None, Some("source-1"), "draft"))
        .expect("comment");
    let mut bold = ui::widget::BlockContent::paragraph("", "plain bold");
    bold.marks = vec![ui::widget::MarkSpan::new(
        6..10,
        ui::widget::marks::Mark::Bold,
    )];
    let clips = tilo_rich_text::Draft::with_blocks(&[bold]).clip_blocks();
    replica
        .apply(set_comment_text_action("c", clips))
        .expect("rewrite");
    let got = &markup(&replica).list_comments()[0];
    assert_eq!(got.body, "plain bold");
    assert_eq!(got.blocks.len(), 1);
    assert_eq!(got.blocks[0].marks.len(), 1);
}

#[test]
fn annotations_round_trip_recolor_reposition_and_delete() {
    let mut replica = planned_replica(3);
    let a = hl(2);
    replica
        .apply(add_annotation_on(&a, "source-2"))
        .expect("highlight");
    let list = markup(&replica).list_annotations();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].page, 2);
    assert_eq!(list[0].color, Swatch::Yellow);
    match &list[0].kind {
        AnnoKind::Markup { quads, .. } => assert!((quads[0].w - 100.0).abs() < 1e-6),
        _ => panic!("expected highlight"),
    }
    replica
        .apply(set_annotation_color_action(&a.id, Swatch::Green))
        .expect("recolor");
    assert_eq!(markup(&replica).list_annotations()[0].color, Swatch::Green);
    replica
        .apply(delete_annotation_action(&a.id))
        .expect("delete");
    assert!(markup(&replica).list_annotations().is_empty());

    // Ink points round-trip exactly.
    let mut replica = planned_replica(1);
    let ink = anno(
        1,
        AnnoKind::ink(vec![(1.0, 2.0), (3.5, 4.25), (10.0, 0.0)]),
        Swatch::Blue,
        "Bob",
    );
    replica
        .apply(add_annotation_on(&ink, "source-1"))
        .expect("ink");
    match &markup(&replica).list_annotations()[0].kind {
        AnnoKind::Ink { strokes } => {
            let points = &strokes[0];
            assert_eq!(points.len(), 3);
            assert!((points[1].0 - 3.5).abs() < 1e-6);
            assert!((points[1].1 - 4.25).abs() < 1e-6);
        }
        _ => panic!("expected ink"),
    }

    // Repositioning moves a note marker ...
    let note = anno(
        1,
        AnnoKind::Note { x: 30.0, y: 40.0 },
        Swatch::Blue,
        "Alice",
    );
    replica
        .apply(add_annotation_on(&note, "source-1"))
        .expect("note");
    let moved = set_annotation_position_user_action(&markup(&replica), &note.id, 120.5, 210.25)
        .expect("a note moves");
    replica.apply(moved).expect("reposition");
    let stored = markup(&replica)
        .annotations
        .iter()
        .find(|a| a.id == note.id)
        .cloned()
        .expect("the note");
    match stored.mark {
        AnnoKind::Note { x, y } => {
            assert!((x - 120.5).abs() < 1e-6, "note x moved");
            assert!((y - 210.25).abs() < 1e-6, "note y moved");
        }
        _ => panic!("expected note"),
    }
    assert_eq!(stored.space, GeomSpace::User);

    // ... but ink is a polyline, not a single anchor: it has no position.
    assert!(
        set_annotation_position_user_action(&markup(&replica), &ink.id, 99.0, 99.0).is_none(),
        "ink is not repositioned as a point"
    );
    match &markup(&replica)
        .list_annotations()
        .iter()
        .find(|a| a.id == ink.id)
        .expect("the ink")
        .kind
    {
        AnnoKind::Ink { strokes } => assert_eq!(strokes[0].len(), 3, "ink polyline preserved"),
        _ => panic!("expected ink"),
    }

    // A note whose position is not a number is refused whole.
    let paths = PdfMarkup::paths().annotations.item(&Id::from_raw(note.id));
    assert!(
        replica
            .apply(paths.mark.set(AnnoKind::Note {
                x: f64::NAN,
                y: 1.0
            }))
            .is_err()
    );
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn comment_threads_round_trip_sort_and_keep_reply_ids_through_merges() {
    let mut replica = planned_replica(1).as_author("Alice");
    let a = hl(1);
    replica
        .apply(add_annotation_on(&a, "source-1"))
        .expect("highlight");
    let id = Comment::new_id();
    replica
        .apply(add_comment_action(
            &id,
            Some(&a.id),
            None,
            "Is this still accurate?",
        ))
        .expect("comment");
    let first_reply = Reply::new_id();
    let mut bob = replica.fork().as_author("Bob");
    bob.apply(add_reply_action_with_id(
        &id,
        &first_reply,
        "Yes, verified.",
    ))
    .expect("reply");
    let got = markup(&bob).list_comments();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].anno_id.as_deref(), Some(a.id.as_str()));
    assert_eq!(
        got[0].author, "Alice",
        "a comment is signed by whoever wrote it"
    );
    assert_eq!(got[0].page, 1, "a comment on an annotation is on its page");
    assert_eq!(got[0].replies.len(), 1);
    assert_eq!(got[0].replies[0].author, "Bob");
    assert_eq!(got[0].replies[0].id, first_reply);
    bob.apply(crate::actions::add_reply_text_action(
        &id,
        tilo_rich_text::plain_clips("Agreed."),
    ))
    .expect("second reply");
    bob.apply(set_comment_resolved_action(&id, true))
        .expect("resolve");
    bob.apply(set_comment_text_action(
        &id,
        tilo_rich_text::plain_clips("Edited: confirmed accurate."),
    ))
    .expect("edit");
    let got = markup(&bob).list_comments();
    assert_eq!(got[0].replies.len(), 2);
    assert!(got[0].resolved);
    assert_eq!(got[0].body, "Edited: confirmed accurate.");

    // Comments list in creation order.
    let mut replica = planned_replica(1);
    for (id, created) in [("a", 30), ("b", 10), ("c", 20)] {
        replica.set_now_ms(created);
        replica
            .apply(add_comment_action(id, None, Some("source-1"), "y"))
            .expect("comment");
    }
    let order: Vec<i64> = markup(&replica)
        .list_comments()
        .iter()
        .map(|c| c.created)
        .collect();
    assert_eq!(order, vec![10, 20, 30]);

    // Reply ids survive reload, retries and concurrent merges.
    let mut base = planned_replica(1);
    base.apply(add_comment_action(
        "thread-1",
        None,
        Some("source-1"),
        "Please review this.",
    ))
    .expect("thread");
    let bytes = base.save();
    let mut left = Replica::<PdfMarkup>::load(&bytes)
        .unwrap()
        .as_author("Grace");
    let mut right = Replica::<PdfMarkup>::load(&bytes).unwrap().as_author("Lin");
    left.apply(add_reply_action_with_id(
        "thread-1",
        "reply-left",
        "Looks good.",
    ))
    .expect("left reply");
    // A retried delivery has the same id and must remain exactly one reply.
    left.apply(add_reply_action_with_id(
        "thread-1",
        "reply-left",
        "Looks good.",
    ))
    .expect("retried reply");
    right
        .apply(add_reply_action_with_id(
            "thread-1",
            "reply-right",
            "One small change.",
        ))
        .expect("right reply");
    left.sync_with(&mut right).expect("sync");
    let replies = &markup(&left).list_comments()[0].replies;
    assert_eq!(replies.len(), 2);
    assert!(replies.iter().any(|reply| reply.id == "reply-left"));
    assert!(replies.iter().any(|reply| reply.id == "reply-right"));
    assert_eq!(markup(&left), markup(&right), "both replicas converge");
    let reloaded = Replica::<PdfMarkup>::load(&left.save()).unwrap();
    let reloaded_comments = markup(&reloaded).list_comments();
    let ids: Vec<&str> = reloaded_comments[0]
        .replies
        .iter()
        .map(|reply| reply.id.as_str())
        .collect();
    assert!(ids.contains(&"reply-left") && ids.contains(&"reply-right"));
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn signatures_round_trip_and_deletes_cascade_only_to_anchored_comments() {
    let mut replica = planned_replica(5);
    let typed = signature(
        &Signature::new_id(),
        5,
        PtRect::new(360.0, 640.0, 180.0, 56.0),
        "Alice Daskju",
        SignatureInk::Typed,
    );
    replica
        .apply(add_signature_on(&typed, "source-5"))
        .expect("typed signature");
    let got = markup(&replica).list_signatures();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].signer, "Alice Daskju");
    assert_eq!(got[0].page, 5);
    assert_eq!(got[0].ink, SignatureInk::Typed);
    replica
        .apply(delete_signature_action(&typed.id))
        .expect("delete");
    assert!(markup(&replica).list_signatures().is_empty());

    let strokes = vec![
        vec![(0.1, 0.5), (0.3, 0.2), (0.5, 0.6)],
        vec![(0.6, 0.4), (0.9, 0.5)],
    ];
    let drawn = signature(
        &Signature::new_id(),
        2,
        PtRect::new(100.0, 100.0, 200.0, 70.0),
        "Grace",
        SignatureInk::Drawn { strokes },
    );
    replica
        .apply(add_signature_on(&drawn, "source-2"))
        .expect("drawn signature");
    let got = markup(&replica).list_signatures();
    assert_eq!(got.len(), 1);
    match &got[0].ink {
        SignatureInk::Drawn { strokes: back } => {
            assert_eq!(back.len(), 2, "both strokes survive");
            assert_eq!(back[0].len(), 3);
            assert!((back[0][1].0 - 0.3).abs() < 1e-2 && (back[0][1].1 - 0.2).abs() < 1e-2);
            assert_eq!(back[1].len(), 2);
        }
        SignatureInk::Typed => panic!("expected drawn ink, got Typed"),
    }

    // Deleting a free comment removes it.
    let mut replica = planned_replica(1);
    let free = Comment::new_id();
    replica
        .apply(add_comment_action(&free, None, Some("source-1"), "x"))
        .expect("comment");
    replica
        .apply(delete_comment_action(&free))
        .expect("delete comment");
    assert!(markup(&replica).list_comments().is_empty());

    // Deleting an annotation removes its anchored comments; an unrelated
    // free comment must survive the delete.
    let a = hl(1);
    replica
        .apply(add_annotation_on(&a, "source-1"))
        .expect("highlight");
    replica
        .apply(add_comment_action(
            &Comment::new_id(),
            Some(&a.id),
            None,
            "pinned",
        ))
        .expect("pinned");
    replica.set_now_ms(1_700_000_000_001);
    replica
        .apply(add_comment_action(
            &Comment::new_id(),
            None,
            Some("source-1"),
            "free",
        ))
        .expect("free");
    assert_eq!(markup(&replica).list_comments().len(), 2);
    replica
        .apply(delete_annotation_action(&a.id))
        .expect("delete annotation");
    assert!(markup(&replica).list_annotations().is_empty());
    let left = markup(&replica).list_comments();
    assert_eq!(
        left.len(),
        1,
        "anchored comment should be deleted with its annotation"
    );
    assert_eq!(left[0].body, "free");
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn page_plan_converges_imports_atomically_and_carries_markup() {
    // Both clients open the same record, past its genesis, before either
    // sees the other: each lays out the plan. The plan has one occurrence
    // per id.
    let genesis = Replica::<PdfMarkup>::new().save();
    let mut left = Replica::<PdfMarkup>::load(&genesis).unwrap();
    let mut right = Replica::<PdfMarkup>::load(&genesis).unwrap();
    left.apply(PdfMarkup::START_PAGE_PLAN.with(3)).unwrap();
    right.apply(PdfMarkup::START_PAGE_PLAN.with(3)).unwrap();
    left.sync_with(&mut right).unwrap();
    let initial = markup(&left).page_plan();
    assert_eq!(
        initial
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["source-1", "source-2", "source-3"]
    );
    assert_eq!(markup(&left), markup(&right));

    let checkpoint = left.save();
    let mut a = Replica::<PdfMarkup>::load(&checkpoint).unwrap();
    let mut b = Replica::<PdfMarkup>::load(&checkpoint).unwrap();
    a.apply(move_page_plan_action("source-2", None)).unwrap();
    b.apply(move_page_plan_action("source-2", Some("source-3")))
        .unwrap();
    a.sync_with(&mut b).unwrap();
    let plan = markup(&a).page_plan();
    let ids: HashSet<&str> = plan.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(ids.len(), plan.len(), "each durable id materialises once");
    assert_eq!(ids.len(), 3);
    assert_eq!(markup(&a).page_plan(), markup(&b).page_plan());

    // Markup anchors follow a page occurrence through move and delete.
    let mut replica = planned_replica(3);
    let annotation = hl(1);
    replica
        .apply(add_annotation_on(&annotation, "source-1"))
        .unwrap();
    replica
        .apply(add_comment_action(
            "comment-on-source-1",
            Some(&annotation.id),
            None,
            "attached",
        ))
        .unwrap();
    replica
        .apply(add_signature_on(
            &signature(
                "signature-on-source-1",
                1,
                PtRect::new(10.0, 10.0, 100.0, 40.0),
                "Alice",
                SignatureInk::Typed,
            ),
            "source-1",
        ))
        .unwrap();
    replica
        .apply(move_page_plan_action("source-1", Some("source-3")))
        .unwrap();
    let moved = markup(&replica);
    assert_eq!(moved.list_annotations()[0].page, 3);
    assert_eq!(moved.list_comments()[0].page, 3);
    assert_eq!(moved.list_signatures()[0].page, 3);
    replica
        .apply(crate::actions::delete_page_plan_action("source-1"))
        .unwrap();
    let deleted = markup(&replica);
    assert!(deleted.list_annotations().is_empty());
    assert!(deleted.list_comments().is_empty());
    assert!(deleted.list_signatures().is_empty());
    // The markup stays in the record, attached to the deleted page.
    let hidden = deleted.annotations_with_hidden();
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].page, 0);
    assert_eq!(
        deleted.annotation_page_id(&hidden[0].id).as_deref(),
        Some("source-1")
    );
    // The last page stays.
    replica
        .apply(crate::actions::delete_page_plan_action("source-2"))
        .unwrap();
    assert!(
        replica
            .apply(crate::actions::delete_page_plan_action("source-3"))
            .is_err(),
        "a document keeps at least one page"
    );

    // Imported pages are atomic and keep their durable identity.
    let mut replica = planned_replica(2);
    let page = PagePlanEntry {
        id: "imported-image".into(),
        source_page: 1,
        imported: Some(ImportedPage {
            hash: "a".repeat(64),
            kind: "image".into(),
            page: 1,
            width_milli: 612_000,
            height_milli: 408_000,
        }),
        rotation: 0,
    };
    replica
        .apply(import_pages_action(Some("source-1"), vec![page.clone()]))
        .unwrap();
    assert_eq!(markup(&replica).page_plan()[1], page);
    // A malformed batch refuses as a whole, including the otherwise valid
    // first entry. It cannot leave a partial shared page plan behind.
    let mut good = page.clone();
    good.id = "good".into();
    let mut bad = page;
    bad.id = "bad".into();
    bad.imported.as_mut().unwrap().hash = "tampered".into();
    assert!(
        replica
            .apply(import_pages_action(None, vec![good, bad]))
            .is_err()
    );
    assert_eq!(markup(&replica).page_plan().len(), 3);
}

fn resource_ctx(
    title: &str,
    page_ids: Vec<String>,
    outline: Vec<(String, usize, u8)>,
) -> PdfResourceCtx {
    let page_count = page_ids.len();
    PdfResourceCtx {
        doc_id: "fixture-document".into(),
        title: title.into(),
        page: 1,
        page_count,
        page_ids,
        page_sources: (1..=page_count).collect(),
        page_imports: vec![None; page_count],
        page_rotations: Vec::new(),
        author: "Ada".into(),
        signature_date: "2026-09-06".into(),
        created_seed: 0,
        search_query: String::new(),
        search_whole_word: false,
        search_case_sensitive: false,
        search_token: search_result_token("", false, false),
        search_pages: Vec::new(),
        outline,
    }
}

/// The record publishes its pages, markup, threads and signatures as items
/// with the verbs each implies; the outline and find hits are live rows
/// beside it, bounded, with omitted hits resolvable by id; outline ids
/// survive reordering but fail closed when changed.
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn document_resources_publish_items_and_bounded_stable_rows() {
    let mut replica = planned_replica(3);
    for edit in crate::scenarios::sample_review() {
        replica.apply(edit).expect("the sample review");
    }
    let document = replica.published().expect("published");
    assert_eq!(document.kind, "pdf.document");
    let doc_actions = action_ids(&document);
    for id in [
        "add-note",
        "add-typed-signature",
        "add-comment",
        "import-pages",
    ] {
        assert!(doc_actions.contains(&id), "{doc_actions:?}");
    }
    let page = find(&document, &item("pages", "source-1")).expect("page item");
    let duplicate = page
        .actions
        .iter()
        .find(|action| action.id == "duplicate-page")
        .expect("duplicate page action");
    assert_eq!(duplicate.kind, OperationKind::Document);
    assert!(duplicate.contract.reversible);
    assert!(action_ids(page).contains(&"delete-page"));

    let anno =
        find(&document, &item("annotations", crate::scenarios::NOTE)).expect("an annotation item");
    let anno_actions = action_ids(anno);
    assert!(
        anno_actions.contains(&"delete-annotation"),
        "{anno_actions:?}"
    );
    let comment =
        find(&document, &item("comments", crate::scenarios::NOTE_COMMENT)).expect("a comment item");
    let cm_actions = action_ids(comment);
    assert!(cm_actions.contains(&"reply"), "{cm_actions:?}");
    assert!(cm_actions.contains(&"delete-comment"), "{cm_actions:?}");
    let state = markup(&replica);
    let signature = state.signatures.first().expect("the sample signs");
    assert!(find(&document, &item("signatures", &signature.id)).is_some());
    assert!(
        PdfMarkup::child(&state, &item("comments", crate::scenarios::NOTE_COMMENT)).is_some(),
        "a thread stays addressable by id for agents and history"
    );

    // The palette offers what a reviewer reaches for.
    let labels: Vec<String> = std::iter::once(&document)
        .chain(document.children.iter())
        .flat_map(|resource| resource.actions.iter().map(|a| a.label.clone()))
        .collect();
    for label in ["Add note", "Delete annotation", "Delete comment"] {
        assert!(labels.iter().any(|l| l == label), "{label}: {labels:?}");
    }

    let mut find_ctx = resource_ctx("Large fixture", Vec::new(), Vec::new());
    find_ctx.search_query = "fixture".into();
    find_ctx.search_token = search_result_token("fixture", false, false);
    find_ctx.search_pages = (1..=SEARCH_RESULT_RESOURCE_LIMIT + 9).collect();
    let find_resource = live_rows(&find_ctx)
        .into_iter()
        .map(LiveRow::into_resource)
        .find(|resource| resource.kind == "pdf.search")
        .expect("non-empty find query has a result resource");
    assert_eq!(find_resource.children.len(), SEARCH_RESULT_RESOURCE_LIMIT);
    let omitted_find = format!(
        "pdf.search-result:{}:{}:{}:{}",
        find_ctx.doc_id,
        find_ctx.search_token,
        SEARCH_RESULT_RESOURCE_LIMIT,
        SEARCH_RESULT_RESOURCE_LIMIT + 1
    );
    assert!(
        live_row(&find_ctx, &omitted_find).is_some(),
        "an omitted find hit remains addressable by its stable query/result id"
    );
    let mut requeried = find_ctx;
    requeried.search_token = search_result_token("other", false, false);
    assert!(live_row(&requeried, &omitted_find).is_none());

    // Outline resource ids survive reordering but fail closed when changed.
    let mut ctx = resource_ctx(
        "Outline fixture",
        vec!["p-1".into(), "p-2".into(), "p-3".into()],
        vec![
            ("Introduction".into(), 1, 0),
            ("Security model".into(), 2, 0),
            ("Security model".into(), 2, 0),
        ],
    );
    let outline = live_rows(&ctx)
        .into_iter()
        .map(LiveRow::into_resource)
        .find(|r| r.kind == "pdf.outline-item")
        .expect("an outline item beside the published document");
    assert!(
        outline
            .actions
            .iter()
            .all(|action| action.kind == OperationKind::View),
        "the outline is a live row with view operations only"
    );
    let introduction = outline_resource_id(&ctx, 0).expect("valid outline entry");
    let first_security = outline_resource_id(&ctx, 1).expect("valid outline entry");
    let duplicate_security = outline_resource_id(&ctx, 2).expect("valid duplicate entry");
    assert_ne!(
        first_security, duplicate_security,
        "duplicates retain identities"
    );
    for id in [&introduction, &first_security, &duplicate_security] {
        assert!(live_row(&ctx, id).is_some(), "outline id {id}");
    }
    ctx.outline.insert(0, ("Preface".into(), 1, 0));
    assert!(
        live_row(&ctx, &first_security).is_some(),
        "inserting another bookmark does not rebind the saved action"
    );
    assert!(live_row(&ctx, &introduction).is_some());
    assert!(live_row(&ctx, &duplicate_security).is_some());
    ctx.outline[1].1 = 3;
    assert!(
        live_row(&ctx, &introduction).is_none(),
        "a changed bookmark fails closed rather than opening another section"
    );
}

/// Signatures and notes through the door sign as their invoker; deleting an
/// annotation removes every comment pinned to it; a markup action is one
/// undo step of its author's.
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn markup_actions_apply_through_the_door_and_undo_as_their_author() {
    let mut replica = planned_replica(2);
    let drawn = Signature {
        id: "sig-action".into(),
        page: 2,
        rect: PtRect::new(10.0, 20.0, 180.0, 56.0),
        signer: "Ada".into(),
        date: "2026-09-05".into(),
        ink: SignatureInk::Drawn {
            strokes: vec![vec![(0.0, 0.5), (1.0, 0.5)]],
        },
        space: GeomSpace::Page,
    };
    replica
        .apply(add_signature_on(&drawn, "source-2"))
        .expect("drawn");
    assert_eq!(markup(&replica).list_signatures()[0].ink, drawn.ink);
    replica
        .apply(delete_signature_action(&drawn.id))
        .expect("delete");
    assert!(markup(&replica).list_signatures().is_empty());

    // The typed signature uses the named signer and signs as the invoker
    // when none is named; one call adds a note with its thread.
    let mut app =
        ui::framework::testing::TestApp::<crate::PdfViewerShell>::new().expect("the reader");
    app.invoke(PdfMarkup::START_PAGE_PLAN.with(3))
        .expect("the plan");
    app.invoke(PdfMarkup::ADD_TYPED_SIGNATURE.with((Some("Grace Hopper".to_owned()), Some(2))))
        .expect("the owner signs");
    app.invoke(PdfMarkup::ADD_TYPED_SIGNATURE.with((None, Some(1))))
        .expect("the owner signs as themselves");
    app.call(
        "add-note",
        serde_json::json!({ "text": "Check the figures", "page": 3 }),
    )
    .expect("an agent adds a note in one call");
    let state = app.state();
    let named = state
        .list_signatures()
        .into_iter()
        .find(|signature| signature.signer == "Grace Hopper")
        .expect("named signer is saved");
    assert_eq!(named.ink, SignatureInk::Typed);
    assert_eq!(named.page, 2);
    let own = state
        .list_signatures()
        .into_iter()
        .find(|signature| signature.page == 1)
        .expect("unnamed signature is saved");
    assert!(
        !own.signer.is_empty(),
        "an unnamed signature is the invoker's"
    );
    let note = state
        .list_comments()
        .into_iter()
        .find(|comment| comment.body == "Check the figures")
        .expect("the note's comment");
    assert_eq!(note.page, 3);
    assert_eq!(note.author, own.signer, "both are signed by the invoker");
    assert!(
        state
            .list_annotations()
            .iter()
            .any(|a| Some(a.id.as_str()) == note.anno_id.as_deref()),
        "the note's marker is on the page"
    );

    // Deleting an annotation that carries a pinned comment.
    let mut replica = planned_replica(3);
    for edit in crate::scenarios::sample_review() {
        replica.apply(edit).expect("the sample review");
    }
    let before = markup(&replica);
    let target = before
        .list_annotations()
        .into_iter()
        .find(|a| {
            before
                .list_comments()
                .iter()
                .any(|c| c.anno_id.as_deref() == Some(a.id.as_str()))
        })
        .expect("a seeded annotation with a pinned comment");
    replica
        .apply(delete_annotation_action(&target.id))
        .expect("delete");
    let after = markup(&replica);
    assert!(
        after.list_annotations().iter().all(|a| a.id != target.id),
        "the annotation is gone"
    );
    assert!(
        after
            .list_comments()
            .iter()
            .all(|c| c.anno_id.as_deref() != Some(target.id.as_str())),
        "and every comment pinned to it went with it"
    );

    // A markup action is one undo step of its author's.
    let mut app =
        ui::framework::testing::TestApp::<crate::PdfViewerShell>::new().expect("the reader");
    app.invoke(PdfMarkup::START_PAGE_PLAN.with(1))
        .expect("the plan");
    let a = anno(
        1,
        AnnoKind::highlight(PtRect::new(70.0, 300.0, 120.0, 12.0)),
        Swatch::Green,
        "You",
    );
    app.invoke(add_annotation_on(&a, "source-1"))
        .expect("the owner highlights");
    assert_eq!(app.state().annotations.len(), 1);
    assert!(app.undo(), "the highlight is an undo step");
    assert!(
        app.state().annotations.is_empty(),
        "undo reverted the added highlight and nothing else"
    );
}

/// Peer B's content backend: its own local content, plus a peer (A) it
/// fetches from. A miss is not served synchronously; it asks the peer,
/// reports the fetch as pending, and installs the bytes (verified against
/// the hash that named them) for a later read. The two peers share nothing.
struct PeerFetchingBackend {
    local: MemoryBackend,
    peer: ContentStore,
    requested: Mutex<BTreeSet<String>>,
}

impl PeerFetchingBackend {
    fn new(peer: ContentStore) -> Self {
        Self {
            local: MemoryBackend::default(),
            peer,
            requested: Mutex::new(BTreeSet::new()),
        }
    }

    /// Ask the peer for `hash`; the answer lands locally on the next
    /// `state` query, like a transfer completing between frames.
    fn want(&self, hash: &str) {
        if let Ok(mut requested) = self.requested.lock() {
            requested.insert(hash.to_string());
        }
    }

    fn settle(&self, hash: &str) {
        let wanted = self
            .requested
            .lock()
            .is_ok_and(|mut requested| requested.remove(hash));
        if wanted && let Some(mut reader) = self.peer.open(hash) {
            let _ = self.local.put(&mut reader, "");
        }
    }
}

impl ContentBackend for PeerFetchingBackend {
    fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
        self.local.put(reader, mime)
    }

    fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
        let found = self.local.open(hash)?;
        if found.is_none() {
            self.want(hash);
        }
        Ok(found)
    }

    fn has(&self, hash: &str) -> bool {
        self.local.has(hash)
    }

    fn state(&self, hash: &str) -> ContentState {
        self.settle(hash);
        if self.local.has(hash) {
            ContentState::Resolved
        } else if self
            .requested
            .lock()
            .is_ok_and(|requested| requested.contains(hash))
        {
            ContentState::Pending
        } else {
            ContentState::Unavailable
        }
    }
}

#[test]
fn imported_pages_replay_identically_and_bytes_travel_through_the_content_store() {
    let mut a = planned_replica(1);
    let mut b = Replica::<PdfMarkup>::load(&a.save()).unwrap();
    let page = PagePlanEntry {
        id: "import-replay-id".into(),
        source_page: 1,
        imported: Some(ImportedPage {
            hash: "b".repeat(64),
            kind: "image".into(),
            page: 1,
            width_milli: 612_000,
            height_milli: 408_000,
        }),
        rotation: 0,
    };
    let envelope = a
        .apply(import_pages_action(Some("source-1"), vec![page]))
        .unwrap()
        .clone();
    b.replay(&envelope).unwrap();
    assert_eq!(a.heads(), b.heads());
    assert_eq!(markup(&a).page_plan(), markup(&b).page_plan());

    // Imported bytes cross between content stores without entering the
    // record.
    let producer = ContentStore::memory();
    let consumer = ContentStore::new(PeerFetchingBackend::new(producer.clone()));
    let bytes = b"content that is deliberately not in Automerge";
    let hash = producer
        .put(bytes)
        .expect("test fixture fits the content quota");
    assert!(producer.has(&hash));
    assert!(!consumer.has(&hash));
    assert_eq!(consumer.state(&hash), ContentState::Unavailable);

    // The import names the content by hash only.
    let mut replica = planned_replica(1);
    replica
        .apply(import_pages_action(
            None,
            vec![PagePlanEntry {
                id: "imported".into(),
                source_page: 1,
                imported: Some(ImportedPage {
                    hash: hash.clone(),
                    kind: "image".into(),
                    page: 1,
                    width_milli: 1_000,
                    height_milli: 1_000,
                }),
                rotation: 0,
            }],
        ))
        .unwrap();
    let saved = replica.save();
    assert!(
        !saved
            .windows(bytes.len())
            .any(|window| window == bytes.as_slice()),
        "the bytes are not in the record"
    );
    assert!(markup(&replica).page_plan().iter().any(|entry| {
        entry
            .imported
            .as_ref()
            .is_some_and(|page| page.hash == hash)
    }));

    // First read is intentionally non-blocking; it asks the peer.
    assert!(consumer.read(&hash).is_none());
    assert_eq!(consumer.state(&hash), ContentState::Resolved);
    assert_eq!(consumer.read(&hash).as_deref(), Some(bytes.as_slice()));
    // Different bytes can never be stored under this address.
    assert_ne!(consumer.put(b"tampered").ok().as_ref(), Some(&hash));
    assert_eq!(consumer.read(&hash).as_deref(), Some(bytes.as_slice()));
}

/// What an agent does for a person is history the person reads: the
/// reader's sentence for each field and verb, naming the thread as it was
/// called, and who did it for whom.
#[test]
fn history_entries_render_as_the_readers_sentences() {
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<crate::PdfViewerShell>::new().expect("the reader");
    app.invoke(PdfMarkup::START_PAGE_PLAN.with(2))
        .expect("the plan");
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = PdfMarkup::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "add-note",
        serde_json::json!({ "text": "Check the figures", "page": 2 }),
    )
    .expect("an agent adds a note");
    let thread = app
        .state()
        .comments
        .first()
        .expect("the note's thread")
        .id
        .clone();
    let resolved = PdfMarkup::paths()
        .comments
        .item(&Id::from_raw(thread.clone()))
        .resolved
        .set(true);
    app.call_by(
        agent.clone(),
        kind,
        "field-edit",
        serde_json::json!({ "edit": resolved }),
    )
    .expect("an agent resolves the thread");
    app.call_by(
        agent.clone(),
        &item("comments", &thread),
        "reply",
        serde_json::json!({ "text": "Done" }),
    )
    .expect("an agent replies");
    app.call_by(
        agent,
        &item("pages", "source-1"),
        "rotate-page",
        serde_json::json!({}),
    )
    .expect("an agent turns a page");

    let entries: Vec<_> = app
        .history()
        .into_iter()
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences = app.sentences(&entries);
    assert_eq!(
        sentences,
        [
            "Added the note “Check the figures”",
            "Resolved Check the figures",
            "Replied “Done” to Check the figures",
            "Turned a page",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada")),
        "the entry names the agent and the person it acted for"
    );
}
