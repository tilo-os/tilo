//! The document's edit records: two-replica convergence, stable anchors
//! through delete and reorder, history and restore, permissions, and a
//! deterministic export that other readers (and pdfium) read back.

use std::collections::HashMap;

use automerge::{AutoCommit, ReadDoc};
use tilo_automerge::ApplyIncoming;
use ui::Widget;

use super::*;
use crate::actions::{
    PdfViewerAction, add_annotation_on, delete_page_plan_action, encode, move_page_plan_action,
    rotate_page_action, set_form_value_action,
};
use crate::doc::PdfDoc;
use crate::model::{FormEntry, GeomSpace, MarkupStyle};

fn apply(doc: &mut AutoCommit, action: &PdfViewerAction) {
    tilo_actions::apply_local(doc, action, encode(action)).expect("action applies");
}

/// A record with a seeded three-page plan, saved and loaded twice: two
/// replicas with their own actors and a shared history.
fn replicas() -> (AutoCommit, AutoCommit) {
    let mut base = AutoCommit::new();
    PdfDoc::ensure_page_plan(&mut base, 3);
    base.commit();
    let bytes = base.save();
    (
        AutoCommit::load(&bytes).expect("left"),
        AutoCommit::load(&bytes).expect("right"),
    )
}

fn highlight(id: &str, page_id: &str, y: f64) -> PdfViewerAction {
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
    add_annotation_on(&a, Some(page_id))
}

fn merge_both(left: &mut AutoCommit, right: &mut AutoCommit) {
    left.merge_incoming(right).expect("merge right into left");
    right.merge_incoming(left).expect("merge left into right");
}

fn ids(doc: &AutoCommit) -> Vec<(String, usize)> {
    PdfDoc::list_annotations(doc)
        .into_iter()
        .map(|a| (a.id, a.page))
        .collect()
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn concurrent_markup_edits_converge_across_replicas() {
    let (mut left, mut right) = replicas();
    apply(&mut left, &highlight("left", "source-2", 100.0));
    apply(&mut right, &highlight("right", "source-2", 300.0));
    merge_both(&mut left, &mut right);
    let merged = ids(&left);
    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(|(_, page)| *page == 2));
    assert_eq!(merged, ids(&right), "both replicas converge on one list");
    // The stored geometry is user space with the text anchor preserved.
    let a = PdfDoc::list_annotations(&left)
        .into_iter()
        .find(|a| a.id == "left")
        .unwrap();
    assert_eq!(a.space, GeomSpace::User);
    assert_eq!(a.chars, Some((10, 20)));
    assert_eq!(a.created_ms, 1_700_000_000_000);

    {
        // Two peers open the document before either has synced anything: each
        // creates the record's containers. Both peers' markup must survive.
        let mut left = AutoCommit::new();
        let mut right = AutoCommit::new();
        apply(&mut left, &highlight("left", "source-1", 100.0));
        apply(&mut right, &highlight("right", "source-1", 200.0));
        apply(
            &mut left,
            &set_form_value_action("name", &FormEntry::Text("Ada".into())),
        );
        apply(
            &mut right,
            &set_form_value_action(
                "agree",
                &FormEntry::Checked {
                    on: true,
                    export: None,
                },
            ),
        );
        merge_both(&mut left, &mut right);
        let all = |doc: &AutoCommit| -> Vec<String> {
            let mut ids: Vec<String> = PdfDoc::list_annotations_with_hidden(doc)
                .into_iter()
                .map(|a| a.id)
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(all(&left), vec!["left", "right"]);
        assert_eq!(all(&left), all(&right));
        assert_eq!(PdfDoc::form_values(&left).len(), 2);
        assert_eq!(PdfDoc::form_values(&left), PdfDoc::form_values(&right));
    }

    {
        let (mut left, mut right) = replicas();
        // Left moves page 1 to the end while right annotates it.
        apply(
            &mut left,
            &move_page_plan_action("source-1", Some("source-3")),
        );
        apply(&mut right, &highlight("on-first", "source-1", 120.0));
        merge_both(&mut left, &mut right);
        let order: Vec<String> = PdfDoc::list_page_plan(&left)
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(order, vec!["source-2", "source-3", "source-1"]);
        assert_eq!(ids(&left), vec![("on-first".to_string(), 3)]);
        assert_eq!(ids(&left), ids(&right));
        // Concurrent moves of the same page still materialise it once.
        let (mut a, mut b) = replicas();
        apply(&mut a, &move_page_plan_action("source-2", None));
        apply(&mut b, &move_page_plan_action("source-2", Some("source-3")));
        merge_both(&mut a, &mut b);
        assert_eq!(PdfDoc::list_page_plan(&a).len(), 3);
        assert_eq!(PdfDoc::list_page_plan(&a), PdfDoc::list_page_plan(&b));
    }

    {
        let mut app = PdfApp::sample();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let base = state.doc.save();
        let mut other = AutoCommit::load(&base).expect("collaborator replica");

        // This user deletes page 2 as one undoable action…
        app.perform_action(&mut state.doc, &delete_page_plan_action("source-2"));
        let author = ui::collaboration::local_user_id();
        let root: *mut dyn Widget = &mut app;
        assert!(ui::undo::record_dispatched_action(
            &mut state,
            root,
            &HashMap::new(),
            &author,
        ));
        // …while a collaborator, not yet seeing the delete, annotates it.
        apply(&mut other, &highlight("late", "source-2", 200.0));
        state
            .doc
            .merge_incoming(&mut other)
            .expect("merge the collaborator");
        other.merge_incoming(&mut state.doc).expect("and back");

        // Defined outcome: the page is gone, its annotation is kept, attached to
        // the deleted page, and not shown anywhere else.
        assert!(ids(&state.doc).iter().all(|(id, _)| id != "late"));
        let hidden: Vec<_> = PdfDoc::list_annotations_with_hidden(&state.doc)
            .into_iter()
            .filter(|a| a.id == "late")
            .collect();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].page, 0);
        assert_eq!(
            PdfDoc::annotation_page_id(&state.doc, "late").as_deref(),
            Some("source-2")
        );
        assert_eq!(ids(&state.doc), ids(&other), "replicas agree");

        // Undoing the delete brings the page back — with the annotation on it.
        state
            .undo_as(Some(&author), None, None)
            .expect("the delete has an undo boundary");
        let restored: Vec<_> = ids(&state.doc)
            .into_iter()
            .filter(|(id, _)| id == "late")
            .collect();
        assert_eq!(restored, vec![("late".to_string(), 2)]);
    }

    {
        let (mut left, mut right) = replicas();
        apply(&mut left, &rotate_page_action("source-1", 90));
        apply(
            &mut right,
            &move_page_plan_action("source-1", Some("source-2")),
        );
        merge_both(&mut left, &mut right);
        let plan = PdfDoc::list_page_plan(&left);
        assert_eq!(plan, PdfDoc::list_page_plan(&right));
        let moved = plan.iter().find(|p| p.id == "source-1").unwrap();
        assert_eq!(
            moved.rotation, 90,
            "rotation is keyed by page id, not list slot"
        );
        // Concurrent rotations resolve as one register too.
        let (mut a, mut b) = replicas();
        apply(&mut a, &rotate_page_action("source-3", 90));
        apply(&mut b, &rotate_page_action("source-3", 180));
        merge_both(&mut a, &mut b);
        assert_eq!(
            PdfDoc::page_rotation(&a, "source-3"),
            PdfDoc::page_rotation(&b, "source-3")
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn form_edits_history_and_read_only_roles_govern_record_changes() {
    let (mut left, mut right) = replicas();
    apply(
        &mut left,
        &set_form_value_action("name", &FormEntry::Text("Ada".into())),
    );
    apply(
        &mut right,
        &set_form_value_action("name", &FormEntry::Text("Grace".into())),
    );
    apply(
        &mut right,
        &set_form_value_action(
            "agree",
            &FormEntry::Checked {
                on: true,
                export: None,
            },
        ),
    );
    merge_both(&mut left, &mut right);
    let l = PdfDoc::form_values(&left);
    let r = PdfDoc::form_values(&right);
    assert_eq!(l, r, "both replicas pick the same winner");
    let winner = l.get("name").cloned().unwrap();
    assert!(
        winner == FormEntry::Text("Ada".into()) || winner == FormEntry::Text("Grace".into()),
        "the value is one of the concurrent writes, never a blend: {winner:?}"
    );
    // The losing write stays in the key's conflict set.
    let Some((_, map)) = left.get(&automerge::ROOT, "form-values").unwrap() else {
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
        &set_form_value_action("name", &FormEntry::Text("Lin".into())),
    );
    right.merge_incoming(&mut left).unwrap();
    assert_eq!(
        PdfDoc::form_values(&right).get("name"),
        Some(&FormEntry::Text("Lin".into()))
    );

    {
        let mut app = PdfApp::sample();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let author = ui::collaboration::local_user_id();
        let root: *mut dyn Widget = &mut app;

        app.perform_action(&mut state.doc, &highlight("first", "source-1", 100.0));
        assert!(ui::undo::record_dispatched_action(
            &mut state,
            root,
            &HashMap::new(),
            &author
        ));
        let history = state.revision_history();
        let after_first = history.heads().to_vec();
        app.perform_action(&mut state.doc, &highlight("second", "source-1", 300.0));
        assert!(ui::undo::record_dispatched_action(
            &mut state,
            root,
            &HashMap::new(),
            &author
        ));
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
                .restore_revision(target, None, None, Some(author.clone()))
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
        use ui::permissions::{Actor, Role};
        let mut app = PdfApp::sample();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let before = PdfDoc::list_annotations_with_hidden(&state.doc).len();

        app.set_permissions(PermissionContext::from_capabilities(
            Actor::person(Some(Role::Viewer), "viewer"),
            Vec::new(),
        ));
        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (70.0, 150.0),
            cur: (360.0, 165.0),
            points: vec![],
        });
        app.commit_drag(&mut state.doc);
        app.create_note(&mut state.doc, 1, (120.0, 300.0));
        app.perform_action(&mut state.doc, &rotate_page_action("source-1", 90));
        app.perform_action(
            &mut state.doc,
            &set_form_value_action("name", &FormEntry::Text("x".into())),
        );
        assert_eq!(
            PdfDoc::list_annotations_with_hidden(&state.doc).len(),
            before,
            "a viewer's markup is refused"
        );
        assert_eq!(PdfDoc::page_rotation(&state.doc, "source-1"), 0);
        assert!(PdfDoc::form_values(&state.doc).is_empty());
        assert!(
            app.status_note
                .as_deref()
                .is_some_and(|n| n.contains("not change")),
            "the refusal is explained"
        );

        // A commenter may reply to a thread but still not mark up the page.
        let thread = PdfDoc::list_comments(&state.doc)
            .first()
            .map(|c| c.id.clone())
            .expect("the sample has a thread");
        app.set_permissions(PermissionContext::from_capabilities(
            Actor::person(Some(Role::Commenter), "commenter"),
            Vec::new(),
        ));
        let replies_before = PdfDoc::list_comments(&state.doc)[0].replies.len();
        app.perform_action(
            &mut state.doc,
            &crate::actions::add_reply_action(&thread, "commenter", "A reply"),
        );
        assert_eq!(
            PdfDoc::list_comments(&state.doc)[0].replies.len(),
            replies_before + 1
        );
        app.perform_action(&mut state.doc, &highlight("nope", "source-1", 50.0));
        assert!(ids(&state.doc).iter().all(|(id, _)| id != "nope"));
    }
}

fn engine_fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/platform/tilo-pdf/tests/fixtures")
        .join(name)
}

#[serial_test::serial(pdfium)]
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn exports_are_deterministic_and_carry_page_operations() {
    eprintln!("phase export_is_deterministic_and_reloads_with_the_records");
    'phase: {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("form.pdf");
        std::fs::copy(engine_fixture("form.pdf"), &path).expect("copy fixture");
        let mut app = PdfApp::open(&path);
        if app.raster.is_none() {
            eprintln!("SKIPPING: no PDF engine on this host");
            break 'phase;
        }
        let mut state = ui::State::new();
        let checkout = ui::record::capability::record_intent(&app).expect("a markup record");
        ui::RecordSet::in_memory().checkout(&mut state, None, &checkout);
        app.sync(&mut state.doc, &state.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();

        // The record pins the bytes it annotates.
        let (pinned, len, _) = PdfDoc::source(&state.doc).expect("source pinned");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(pinned, tilo_content::hash_bytes(&bytes));
        assert_eq!(len, bytes.len() as u64);
        assert!(
            ui::record::blob_references(&state.doc)
                .iter()
                .any(|b| b.hash == pinned && b.relation == "source")
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
        app.commit_drag(&mut state.doc);
        app.create_note(&mut state.doc, 1, (250.0, 30.0));
        app.editing.as_mut().unwrap().set_text("Please confirm");
        app.commit_edit(&mut state.doc);
        let name = app
            .field_widgets_on(1)
            .into_iter()
            .find(|w| w.name == "name")
            .expect("the name field is on page 1");
        app.activate_field(&mut state.doc, &name);
        app.editing.as_mut().unwrap().set_text("Grace Hopper");
        app.commit_edit(&mut state.doc);
        let agree = app
            .field_widgets_on(1)
            .into_iter()
            .find(|w| w.name == "agree")
            .unwrap();
        app.activate_field(&mut state.doc, &agree);
        let colour = app
            .field_widgets_on(1)
            .into_iter()
            .find(|w| w.name == "colour")
            .unwrap();
        app.activate_field(&mut state.doc, &colour);
        assert_eq!(
            PdfDoc::form_values(&state.doc).get("colour"),
            Some(&FormEntry::Choice("Green".into())),
            "a choice field steps to its next option"
        );
        app.reload(&state.doc);

        // The same records on the same bytes export byte-identically.
        let out1 = dir.path().join("one.pdf");
        let out2 = dir.path().join("two.pdf");
        assert!(app.save_annotated_to(&out1), "{:?}", app.status_note);
        assert!(app.save_annotated_to(&out2), "{:?}", app.status_note);
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
        let mut app = PdfApp::open(&path);
        if app.raster.is_none() {
            eprintln!("SKIPPING: no PDF engine on this host");
            break 'phase;
        }
        let mut state = ui::State::new();
        let checkout = ui::record::capability::record_intent(&app).expect("a markup record");
        ui::RecordSet::in_memory().checkout(&mut state, None, &checkout);
        app.sync(&mut state.doc, &state.root);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();

        // Highlight "needle" on page 3 from its text, then rotate that page,
        // move it first and delete page 2.
        let text = app.ensure_raster_text(3).expect("text layer");
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
        app.author_annotation(&mut state.doc, &a);
        app.perform_action(&mut state.doc, &rotate_page_action("source-3", 90));
        app.perform_action(&mut state.doc, &move_page_plan_action("source-3", None));
        app.perform_action(&mut state.doc, &delete_page_plan_action("source-2"));
        app.sync(&mut state.doc, &state.root);
        let stored = PdfDoc::list_annotations(&state.doc);
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].page, 1,
            "the highlight follows its page to the front"
        );
        assert!(stored[0].chars.is_some(), "text-anchored");

        // On screen the page is rotated and the highlight still covers the word.
        let map = app.page_map(1);
        assert_eq!(map.effective.rotation, tilo_pdf::Rotation::Cw90);
        let bitmap = app
            .raster
            .as_mut()
            .unwrap()
            .render(0, 400)
            .expect("rotated page renders");
        assert!(
            bitmap.height > bitmap.width,
            "the landscape page renders portrait once rotated ({}x{})",
            bitmap.width,
            bitmap.height
        );

        let out = dir.path().join("edited.pdf");
        assert!(app.save_annotated_to(&out), "{:?}", app.status_note);
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
