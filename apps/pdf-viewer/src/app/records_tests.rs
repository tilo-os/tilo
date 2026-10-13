//! The document's edit records: two-replica convergence, stable anchors
//! through delete and reorder, history and restore, permissions, and a
//! deterministic export that other readers (and pdfium) read back.

use automerge::{AutoCommit, ReadDoc};
use tilo_automerge::ApplyIncoming;
use ui::widget::Edits;

use super::*;
use crate::actions::{
    add_annotation_on, delete_page_plan_action, move_page_plan_action, rotate_page_action,
    set_form_value_action,
};
use crate::markup::PdfMarkup;
use crate::model::{FormEntry, GeomSpace, MarkupStyle};
use ui::tilo_resource::Invoke;

fn apply(doc: &mut AutoCommit, action: Invoke<PdfMarkup>) {
    PdfMarkup::apply_to(doc, action);
}

/// A record with a seeded three-page plan, saved and loaded twice: two
/// replicas with their own actors and a shared history.
fn replicas() -> (AutoCommit, AutoCommit) {
    let bytes = PdfMarkup::planned(3);
    (
        AutoCommit::load(&bytes).expect("left"),
        AutoCommit::load(&bytes).expect("right"),
    )
}

fn highlight(id: &str, page_id: &str, y: f64) -> Invoke<PdfMarkup> {
    let a = Annotation {
        id: id.to_string(),
        chars: Some((10, 20)),
        created_ms: 1_700_000_000_000,
        space: GeomSpace::User,
        ..Annotation::new(
            1,
            AnnoKind::Markup {
                style: MarkupStyle::Highlight,
                quads: vec![PtRect::new(72.0, y, 200.0, 12.0)],
            },
            Swatch::Yellow,
            "Ada",
        )
    };
    add_annotation_on(&a, page_id)
}

fn merge_both(left: &mut AutoCommit, right: &mut AutoCommit) {
    left.merge_incoming(right).expect("merge right into left");
    right.merge_incoming(left).expect("merge left into right");
}

fn ids(doc: &AutoCommit) -> Vec<(String, usize)> {
    PdfMarkup::of(doc)
        .list_annotations()
        .into_iter()
        .map(|a| (a.id, a.page))
        .collect()
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn concurrent_markup_edits_converge_across_replicas() {
    let (mut left, mut right) = replicas();
    apply(&mut left, highlight("left", "source-2", 100.0));
    apply(&mut right, highlight("right", "source-2", 300.0));
    merge_both(&mut left, &mut right);
    let merged = ids(&left);
    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(|(_, page)| *page == 2));
    assert_eq!(merged, ids(&right), "both replicas converge on one list");
    // The stored geometry is user space with the text anchor preserved.
    let a = PdfMarkup::of(&left)
        .list_annotations()
        .into_iter()
        .find(|a| a.id == "left")
        .unwrap();
    assert_eq!(a.space, GeomSpace::User);
    assert_eq!(a.chars, Some((10, 20)));
    assert_eq!(a.created_ms, 1_700_000_000_000);

    {
        // Two peers open the document before either has synced anything past
        // its genesis: each lays out the page plan. Both peers' markup must
        // survive.
        let mut genesis = ui::tilo_resource::Replica::<PdfMarkup>::new();
        let bytes = genesis.save();
        let mut left = AutoCommit::load(&bytes).expect("left");
        let mut right = AutoCommit::load(&bytes).expect("right");
        apply(&mut left, PdfMarkup::START_PAGE_PLAN.with(2));
        apply(&mut right, PdfMarkup::START_PAGE_PLAN.with(2));
        apply(&mut left, highlight("left", "source-1", 100.0));
        apply(&mut right, highlight("right", "source-1", 200.0));
        apply(
            &mut left,
            set_form_value_action("name", &FormEntry::Text("Ada".into())),
        );
        apply(
            &mut right,
            set_form_value_action(
                "agree",
                &FormEntry::Checked {
                    on: true,
                    export: None,
                },
            ),
        );
        merge_both(&mut left, &mut right);
        let all = |doc: &AutoCommit| -> Vec<String> {
            let mut ids: Vec<String> = PdfMarkup::of(doc)
                .annotations_with_hidden()
                .into_iter()
                .map(|a| a.id)
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(all(&left), vec!["left", "right"]);
        assert_eq!(all(&left), all(&right));
        assert_eq!(PdfMarkup::of(&left).form_values.len(), 2);
        assert_eq!(
            PdfMarkup::of(&left).form_values,
            PdfMarkup::of(&right).form_values
        );
    }

    {
        let (mut left, mut right) = replicas();
        // Left moves page 1 to the end while right annotates it.
        apply(
            &mut left,
            move_page_plan_action("source-1", Some("source-3")),
        );
        apply(&mut right, highlight("on-first", "source-1", 120.0));
        merge_both(&mut left, &mut right);
        let order: Vec<String> = PdfMarkup::of(&left)
            .page_plan()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(order, vec!["source-2", "source-3", "source-1"]);
        assert_eq!(ids(&left), vec![("on-first".to_string(), 3)]);
        assert_eq!(ids(&left), ids(&right));
        // Concurrent moves of the same page still materialise it once.
        let (mut a, mut b) = replicas();
        apply(&mut a, move_page_plan_action("source-2", None));
        apply(&mut b, move_page_plan_action("source-2", Some("source-3")));
        merge_both(&mut a, &mut b);
        assert_eq!(PdfMarkup::of(&a).page_plan().len(), 3);
        assert_eq!(PdfMarkup::of(&a).page_plan(), PdfMarkup::of(&b).page_plan());
    }

    {
        let mut app =
            ui::framework::testing::TestApp::<crate::PdfViewerShell>::new().expect("the reader");
        app.invoke(PdfMarkup::START_PAGE_PLAN.with(3))
            .expect("the plan");
        let mut other = app.peer_document();

        // This user deletes page 2 as one undoable action…
        app.invoke(delete_page_plan_action("source-2"))
            .expect("the owner deletes a page");
        // …while a collaborator, not yet seeing the delete, annotates it.
        apply(&mut other, highlight("late", "source-2", 200.0));
        app.merge_peer(&mut other).expect("merge the collaborator");
        let doc = app.read_document(Clone::clone);

        // Defined outcome: the page is gone, its annotation is kept, attached to
        // the deleted page, and not shown anywhere else.
        assert!(ids(&doc).iter().all(|(id, _)| id != "late"));
        let hidden: Vec<_> = PdfMarkup::of(&doc)
            .annotations_with_hidden()
            .into_iter()
            .filter(|a| a.id == "late")
            .collect();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].page, 0);
        assert_eq!(
            PdfMarkup::of(&doc).annotation_page_id("late").as_deref(),
            Some("source-2")
        );

        // Undoing the delete brings the page back — with the annotation on it.
        assert!(app.undo(), "the delete is one undo step");
        let restored: Vec<_> = app
            .read_document(ids)
            .into_iter()
            .filter(|(id, _)| id == "late")
            .collect();
        assert_eq!(restored, vec![("late".to_string(), 2)]);
    }

    {
        let (mut left, mut right) = replicas();
        apply(&mut left, rotate_page_action("source-1", 90));
        apply(
            &mut right,
            move_page_plan_action("source-1", Some("source-2")),
        );
        merge_both(&mut left, &mut right);
        let plan = PdfMarkup::of(&left).page_plan();
        assert_eq!(plan, PdfMarkup::of(&right).page_plan());
        let moved = plan.iter().find(|p| p.id == "source-1").unwrap();
        assert_eq!(
            moved.rotation, 90,
            "rotation is keyed by page id, not list slot"
        );
        // Concurrent rotations resolve as one register too.
        let (mut a, mut b) = replicas();
        apply(&mut a, rotate_page_action("source-3", 90));
        apply(&mut b, rotate_page_action("source-3", 180));
        merge_both(&mut a, &mut b);
        assert_eq!(
            PdfMarkup::of(&a).page_rotation("source-3"),
            PdfMarkup::of(&b).page_rotation("source-3")
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn form_edits_history_and_read_only_roles_govern_record_changes() {
    let (mut left, mut right) = replicas();
    apply(
        &mut left,
        set_form_value_action("name", &FormEntry::Text("Ada".into())),
    );
    apply(
        &mut right,
        set_form_value_action("name", &FormEntry::Text("Grace".into())),
    );
    apply(
        &mut right,
        set_form_value_action(
            "agree",
            &FormEntry::Checked {
                on: true,
                export: None,
            },
        ),
    );
    merge_both(&mut left, &mut right);
    let l = PdfMarkup::of(&left).form_values;
    let r = PdfMarkup::of(&right).form_values;
    assert_eq!(l, r, "both replicas pick the same winner");
    let winner = l.get("name").cloned().unwrap();
    assert!(
        winner == FormEntry::Text("Ada".into()) || winner == FormEntry::Text("Grace".into()),
        "the value is one of the concurrent writes, never a blend: {winner:?}"
    );
    // The losing write stays in the key's conflict set.
    let Some((_, map)) = left.get(&automerge::ROOT, "form_values").unwrap() else {
        panic!("form values map")
    };
    assert_eq!(left.get_all(&map, "name").unwrap().len(), 2);
    assert_eq!(
        l.get("agree"),
        Some(&FormEntry::Checked {
            on: true,
            export: None
        })
    );
    // A later write supersedes both.
    apply(
        &mut left,
        set_form_value_action("name", &FormEntry::Text("Lin".into())),
    );
    right.merge_incoming(&mut left).unwrap();
    assert_eq!(
        PdfMarkup::of(&right).form_values.get("name"),
        Some(&FormEntry::Text("Lin".into()))
    );

    {
        let mut app = crate::scenarios::sample_reader();
        let mut state = crate::scenarios::sample_state();
        app.sync_settled(&mut state.doc, &state.root);
        let author = ui::collaboration::local_user_id();

        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            highlight("first", "source-1", 100.0),
        );
        let history = state.revision_history();
        let after_first = history.heads().to_vec();
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            highlight("second", "source-1", 300.0),
        );
        assert!(ids(&state.doc).iter().any(|(id, _)| id == "second"));

        // Every edit is a revision with an author and a time in the document's
        // own history.
        let history = state.revision_history();
        let latest = history
            .revisions()
            .iter()
            .find(|r| history.heads().contains(&r.id))
            .expect("the latest revision");
        assert!(latest.timestamp.is_some(), "edits are time-stamped");
        assert!(!latest.actor.is_empty(), "edits carry their author's actor");

        // Restoring the version after the first highlight drops the second as a
        // new, undoable change; the later version stays in history.
        let target = after_first.first().copied().expect("a head");
        assert!(
            state
                .restore_revision(target, None, None, Some(author))
                .expect("known revision")
        );
        let now: Vec<String> = ids(&state.doc).into_iter().map(|(id, _)| id).collect();
        assert!(now.contains(&"first".to_string()));
        assert!(!now.contains(&"second".to_string()));
        assert!(
            state.revision_history().revisions().len() > history.revisions().len(),
            "the restore is recorded as a new revision"
        );
    }

    {
        let mut app = crate::scenarios::sample_reader();
        let mut state = crate::scenarios::sample_state();
        app.sync_settled(&mut state.doc, &state.root);
        let before = PdfMarkup::of(&state.doc).annotations_with_hidden().len();

        // The host says the participant may only view.
        let as_role = |role| {
            ui::framework::testing::dispatcher::<crate::PdfMarkup>(APP_ID).with_permissions(
                ui::permissions::LivePermissions::fixed(
                    ui::permissions::PermissionContext::member("participant", role),
                ),
            )
        };
        app.attach_framework(as_role(ui::permissions::Role::Viewer));
        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (70.0, 150.0),
            cur: (360.0, 165.0),
            points: vec![],
        });
        app.commit_drag(&mut Edits::lend(&mut state.doc));
        app.create_note(&mut Edits::lend(&mut state.doc), 1, (120.0, 300.0));
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            rotate_page_action("source-1", 90),
        );
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            set_form_value_action("name", &FormEntry::Text("x".into())),
        );
        assert_eq!(
            PdfMarkup::of(&state.doc).annotations_with_hidden().len(),
            before,
            "a viewer's markup is refused"
        );
        assert_eq!(PdfMarkup::of(&state.doc).page_rotation("source-1"), 0);
        assert!(PdfMarkup::of(&state.doc).form_values.is_empty());
        assert!(
            app.status_note
                .as_deref()
                .is_some_and(|n| n.contains("view only")),
            "the refusal is explained"
        );

        // A commenter may reply to a thread but still not mark up the page.
        app.attach_framework(as_role(ui::permissions::Role::Commenter));
        let thread = PdfMarkup::of(&state.doc)
            .list_comments()
            .first()
            .map(|c| c.id.clone())
            .expect("the sample has a thread");
        let replies_before = PdfMarkup::of(&state.doc).list_comments()[0].replies.len();
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            crate::actions::add_reply_action(&thread, "A reply"),
        );
        assert_eq!(
            PdfMarkup::of(&state.doc).list_comments()[0].replies.len(),
            replies_before + 1,
            "a commenter's reply is added"
        );
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            highlight("nope", "source-1", 50.0),
        );
        assert!(
            ids(&state.doc).iter().all(|(id, _)| id != "nope"),
            "a commenter's markup is refused"
        );
    }

    // Through the host door too: the comment's reply action needs only the
    // right to comment.
    {
        use ui::framework::testing::TestApp;
        let mut app = TestApp::<crate::PdfViewerShell>::new().expect("the reader");
        app.invoke(PdfMarkup::START_PAGE_PLAN.with(3))
            .expect("the plan");
        for edit in crate::scenarios::sample_review() {
            app.invoke(Invoke::from(edit)).expect("the owner reviews");
        }
        let thread = crate::scenarios::NOTE_COMMENT;
        let item = ui::tilo_resource::items::item_resource_id::<PdfMarkup>(
            "comments",
            &ui::tilo_resource::Id::from_raw(thread),
        );
        app.call_on(&item, "reply", serde_json::json!({ "text": "First" }))
            .expect("the owner replies");
        app.set_role(ui::permissions::Role::Commenter);
        app.call_on(&item, "reply", serde_json::json!({ "text": "Agreed" }))
            .expect("a commenter replies");
        assert!(
            app.state()
                .comments
                .iter()
                .any(|c| c.id == thread && c.replies.values().any(|r| r.text == "Agreed"))
        );
        assert!(
            app.invoke(PdfMarkup::ADD_NOTE.with(("Mine".to_owned(), Some(1))))
                .is_err(),
            "a commenter may not add a note to the page"
        );
        // A typed edit needs what it changes: a reply commenting, a highlight
        // writing.
        app.invoke(crate::actions::add_reply_action(thread, "Typed"))
            .expect("a commenter's typed reply");
        assert!(
            app.invoke(highlight("typed", "source-1", 60.0)).is_err(),
            "a commenter's typed highlight is refused"
        );
        assert!(app.state().annotations.iter().all(|a| a.id != "typed"));
    }
}

fn engine_fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("OUT_DIR")).join(name)
}

#[serial_test::serial(pdfium)]
#[test]
fn exports_are_deterministic_and_carry_page_operations() {
    eprintln!("phase export_is_deterministic_and_reloads_with_the_records");
    'phase: {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("form.pdf");
        std::fs::copy(engine_fixture("form.pdf"), &path).expect("copy fixture");
        let Some(mut app) = tests::open_settled(&path) else {
            break 'phase;
        };
        let mut state = ui::State::new();
        tests::settle_record(&mut app, &mut state);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();

        // The record pins the bytes it annotates.
        let pinned = PdfMarkup::of(&state.doc)
            .pinned_source()
            .expect("source pinned");
        let (pinned, len) = (pinned.hash, pinned.len);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(pinned, tilo_content::hash_bytes(&bytes));
        assert_eq!(len, bytes.len() as u64);
        assert!(
            matches!(
                PdfMarkup::of(&state.doc).source,
                ui::tilo_resource::Bytes::Blob { len: stored, .. } if stored == len
            ),
            "the record references the bytes as a blob"
        );

        // Markup through the reader's own paths: an ink stroke and a note on the
        // page, plus form values.
        app.tool = Tool::Ink;
        app.drag = Some(DragState {
            tool: Tool::Ink,
            page: 1,
            start: (40.0, 40.0),
            cur: (120.0, 60.0),
            points: vec![(40.0, 40.0), (80.0, 70.0), (120.0, 40.0)],
        });
        app.commit_drag(&mut Edits::lend(&mut state.doc));
        app.create_note(&mut Edits::lend(&mut state.doc), 1, (250.0, 30.0));
        app.editing.as_mut().unwrap().set_text("Please confirm");
        app.commit_edit(&mut Edits::lend(&mut state.doc));
        let name = app
            .field_widgets_on(1)
            .into_iter()
            .find(|w| w.name == "name")
            .expect("the name field is on page 1");
        app.activate_field(&mut Edits::lend(&mut state.doc), &name);
        app.editing.as_mut().unwrap().set_text("Grace Hopper");
        app.commit_edit(&mut Edits::lend(&mut state.doc));
        let agree = app
            .field_widgets_on(1)
            .into_iter()
            .find(|w| w.name == "agree")
            .unwrap();
        app.activate_field(&mut Edits::lend(&mut state.doc), &agree);
        let colour = app
            .field_widgets_on(1)
            .into_iter()
            .find(|w| w.name == "colour")
            .unwrap();
        app.activate_field(&mut Edits::lend(&mut state.doc), &colour);
        assert_eq!(
            PdfMarkup::of(&state.doc).form_values.get("colour"),
            Some(&FormEntry::Choice("Green".into())),
            "a choice field steps to its next option"
        );
        app.reload(&state.doc);

        // The same records on the same bytes export byte-identically.
        let out1 = dir.path().join("one.pdf");
        let out2 = dir.path().join("two.pdf");
        assert!(
            tests::save_settled(&mut app, &out1),
            "{:?}",
            app.status_note
        );
        assert!(
            tests::save_settled(&mut app, &out2),
            "{:?}",
            app.status_note
        );
        let one = std::fs::read(&out1).unwrap();
        assert_eq!(one, std::fs::read(&out2).unwrap(), "deterministic export");
        assert!(
            one.starts_with(&bytes),
            "an incremental update of the original"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "the original stays untouched"
        );

        // Reloaded, the export carries real annotations and field values.
        let engine = tilo_pdf::Engine::shared().expect("engine");
        let reloaded = engine.open_bytes(&one).expect("reopen");
        let annotations = reloaded.annotations(0).unwrap();
        assert!(
            annotations
                .iter()
                .any(|a| a.kind == tilo_pdf::AnnotationKind::Ink)
        );
        assert!(annotations.iter().any(|a| {
            a.kind == tilo_pdf::AnnotationKind::Text && a.contents.contains("Please confirm")
        }));
        let fields = reloaded.form_fields().unwrap();
        let field = |n: &str| fields.iter().find(|f| f.name == n).unwrap().clone();
        assert_eq!(field("name").value, "Grace Hopper");
        assert!(field("agree").widgets[0].checked);
        assert_eq!(field("colour").value, "Green");
        // The ink landed where it was drawn: its first point maps back to the
        // card point the stroke started at.
        let ink = annotations
            .iter()
            .find(|a| a.kind == tilo_pdf::AnnotationKind::Ink)
            .unwrap();
        let first = ink.ink[0][0];
        assert!((f64::from(first.x) - 40.0).abs() < 0.5 && (f64::from(first.y) - 40.0).abs() < 0.5);
    }

    eprintln!("phase rotate_reorder_and_delete_export_as_page_operations");
    'phase: {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("multipage.pdf");
        std::fs::copy(engine_fixture("multipage.pdf"), &path).expect("copy fixture");
        let Some(mut app) = tests::open_settled(&path) else {
            break 'phase;
        };
        let mut state = ui::State::new();
        tests::settle_record(&mut app, &mut state);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();

        // Highlight "needle" on page 3 from its text, then rotate that page,
        // move it first and delete page 2.
        let text = tests::text_layer(&mut app, &mut state, 3);
        let hit = text
            .search(3, "needle", crate::model::SearchOptions::default())
            .into_iter()
            .next()
            .expect("needle on page 3");
        let (start, end) = (hit.char_start, hit.char_start + hit.char_len);
        let quads = text.bands_for_range(start, end);
        let a = Annotation {
            chars: text.source_range(start, end),
            ..Annotation::new(
                3,
                AnnoKind::Markup {
                    style: MarkupStyle::Highlight,
                    quads,
                },
                Swatch::Yellow,
                "Ada",
            )
        };
        app.author_annotation(&mut Edits::lend(&mut state.doc), &a);
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            rotate_page_action("source-3", 90),
        );
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            move_page_plan_action("source-3", None),
        );
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            delete_page_plan_action("source-2"),
        );
        tests::settle_until(&mut app, &mut state, |app| {
            app.pending_worker_plan.is_none()
        });
        let stored = PdfMarkup::of(&state.doc).list_annotations();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].page, 1,
            "the highlight follows its page to the front"
        );
        assert!(stored[0].chars.is_some(), "text-anchored");

        // On screen the page is rotated and the highlight still covers the word.
        let map = app.page_map(1);
        assert_eq!(map.effective.rotation, tilo_pdf::Rotation::Cw90);
        let key = (0, crate::source::RasterBackend::bucket_for(400));
        let document_id = app.raster_document.as_ref().expect("open").id;
        app.raster_handle
            .as_ref()
            .expect("the PDF worker")
            .request_render(crate::source::RenderRequest {
                document_id,
                page: 0,
                target_width: 400,
            })
            .expect("the render is queued");
        tests::drain_until(&mut app, |app| app.raster_images.contains_key(&key));
        let bitmap = app.raster_images[&key].clone();
        assert!(
            bitmap.height > bitmap.width,
            "the landscape page renders portrait once rotated ({}x{})",
            bitmap.width,
            bitmap.height
        );

        let out = dir.path().join("edited.pdf");
        assert!(tests::save_settled(&mut app, &out), "{:?}", app.status_note);
        let engine = tilo_pdf::Engine::shared().expect("engine");
        let reloaded = engine.open_bytes(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(reloaded.page_count(), 2);
        assert_eq!(reloaded.page(0).unwrap().rotation, tilo_pdf::Rotation::Cw90);
        let layer = reloaded.text(0).unwrap();
        let found = layer.search("needle", tilo_pdf::SearchOptions::default());
        assert_eq!(found.len(), 1, "page 3 is now first");
        let highlight = reloaded
            .annotations(0)
            .unwrap()
            .into_iter()
            .find(|a| a.kind == tilo_pdf::AnnotationKind::Highlight)
            .expect("the highlight exported");
        // The exported quad covers the word the text layer finds after reload.
        let word = found[0].rects[0];
        let quad = highlight.quads[0];
        let overlap = quad.intersects(&word);
        assert!(overlap, "quad {quad:?} covers the word {word:?}");
    }
}
