//! Markup that was already in the PDF: taken over as records on first open,
//! edited like any other markup, exported without duplicates; free text
//! authored in the reader; comment threads as reply annotations.

use automerge::AutoCommit;
use tilo_automerge::ApplyIncoming;
use ui::Widget;
use ui::widget::Edits;

use super::*;
use crate::markup::PdfMarkup;
use crate::model::FormEntry;

fn engine_fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("OUT_DIR")).join(name)
}

/// The fixture page the tests mark up (`multipage.pdf`'s second page).
const PAGE: usize = 1;

fn note(at: tilo_pdf::Point, name: &str, author: &str, contents: &str) -> tilo_pdf::NewAnnotation {
    tilo_pdf::NewAnnotation {
        page: PAGE,
        kind: tilo_pdf::NewAnnotationKind::Note { at },
        color: tilo_pdf::Rgba::new(255, 200, 0, 255),
        author: author.into(),
        contents: contents.into(),
        name: name.into(),
        modified: "D:20260101120000Z".into(),
        in_reply_to: None,
        reply_type: tilo_pdf::ReplyType::Reply,
    }
}

/// A Typst-generated document that another reader already annotated: a
/// highlight with a comment, a note with a one-reply thread, free text and
/// ink, written as real PDF annotations.
fn annotated_fixture(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let engine = tilo_pdf::Engine::shared().ok()?;
    let source = std::fs::read(engine_fixture("multipage.pdf")).ok()?;
    let doc = engine.open_bytes(&source).ok()?;
    let g = doc.page(PAGE)?;
    let layer = doc.text(PAGE).ok()?;
    let quads: Vec<tilo_pdf::Rect> = layer
        .search("needle", tilo_pdf::SearchOptions::default())
        .first()?
        .rects
        .iter()
        .map(|r| g.rect_to_user(*r))
        .collect();
    let mut reply = note(
        g.to_user(tilo_pdf::Point::new(250.0, 30.0)),
        "reply-1",
        "Bo",
        "Agreed",
    );
    reply.in_reply_to = Some("note-1".into());
    let edits = tilo_pdf::Edits {
        annotations: vec![
            tilo_pdf::NewAnnotation {
                page: PAGE,
                kind: tilo_pdf::NewAnnotationKind::Highlight { quads },
                color: tilo_pdf::Rgba::new(10, 200, 90, 255),
                author: "Ada".into(),
                contents: "Original".into(),
                name: "hl-1".into(),
                modified: String::new(),
                in_reply_to: None,
                reply_type: tilo_pdf::ReplyType::Reply,
            },
            note(
                g.to_user(tilo_pdf::Point::new(250.0, 30.0)),
                "note-1",
                "Ada",
                "Check this",
            ),
            reply,
            tilo_pdf::NewAnnotation {
                page: PAGE,
                kind: tilo_pdf::NewAnnotationKind::FreeText {
                    rect: g.rect_to_user(tilo_pdf::Rect::from_xywh(20.0, 110.0, 130.0, 40.0)),
                    font_size: 12.0,
                },
                color: tilo_pdf::Rgba::new(200, 0, 0, 255),
                author: "Ada".into(),
                contents: "See chapter two".into(),
                name: "ft-1".into(),
                modified: String::new(),
                in_reply_to: None,
                reply_type: tilo_pdf::ReplyType::Reply,
            },
            tilo_pdf::NewAnnotation {
                page: PAGE,
                kind: tilo_pdf::NewAnnotationKind::Ink {
                    strokes: vec![
                        vec![
                            g.to_user(tilo_pdf::Point::new(170.0, 120.0)),
                            g.to_user(tilo_pdf::Point::new(200.0, 140.0)),
                        ],
                        vec![
                            g.to_user(tilo_pdf::Point::new(210.0, 120.0)),
                            g.to_user(tilo_pdf::Point::new(240.0, 140.0)),
                        ],
                    ],
                    width: 2.0,
                },
                color: tilo_pdf::Rgba::new(0, 0, 255, 255),
                author: "Ada".into(),
                contents: String::new(),
                name: "ink-1".into(),
                modified: String::new(),
                in_reply_to: None,
                reply_type: tilo_pdf::ReplyType::Reply,
            },
        ],
        ..tilo_pdf::Edits::default()
    };
    let bytes = doc.export(&edits, &[]).ok()?;
    let path = dir.join("reviewed.pdf");
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

/// Open `path` in a reader with its markup record checked out, synced and
/// laid out.
fn open(path: &std::path::Path) -> Option<(PdfApp, ui::State)> {
    let mut app = tests::open_settled(path)?;
    let mut state = crate::scenarios::record_state();
    tests::settle_record(&mut app, &mut state);
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.recompute_regions();
    app.sync_viewport_frame();
    app.sync_settled(&mut state.doc, &state.root);
    Some((app, state))
}

/// Whether appearance text can be checked here. It is set in the pinned fonts
/// compiled into the sandboxed helper; the in-process engine of a
/// development build has none, so these tests share the helper's gate.
fn helper_sets_appearance_text() -> bool {
    const WHAT: &str = "appearance text in the helper's pinned fonts";
    match tilo_pdf::sandboxed_for_test(tilo_pdf::Engine::shared(), WHAT) {
        tilo_pdf::HelperGate::Sandboxed(_) => true,
        tilo_pdf::HelperGate::StandDown => false,
        tilo_pdf::HelperGate::Failed(e) => {
            eprintln!("SKIPPING {WHAT}: no PDF engine on this host ({e})");
            false
        }
    }
}

fn count(annots: &[tilo_pdf::AnnotationInfo], kind: &tilo_pdf::AnnotationKind) -> usize {
    annots.iter().filter(|a| &a.kind == kind).count()
}

#[serial_test::serial(pdfium)]
#[test]
fn file_markup_and_form_values_are_adopted_once_and_exported_without_duplicates() {
    eprintln!("phase existing_markup_is_taken_over_edited_and_exported_without_duplicates");
    'phase: {
        let dir = tempfile::tempdir().expect("temp dir");
        let Some(path) = annotated_fixture(dir.path()) else {
            eprintln!("SKIPPING: no PDF engine on this host");
            break 'phase;
        };
        let Some((mut app, mut state)) = open(&path) else {
            eprintln!("SKIPPING: no PDF engine on this host");
            break 'phase;
        };
        let doc = &mut state.doc;

        // First open: every piece of markup became a record.
        let annotations = PdfMarkup::of(doc)
            .expect("the PDF markup reads")
            .list_annotations();
        assert_eq!(annotations.len(), 4, "{annotations:#?}");
        let kinds: Vec<&str> = annotations.iter().map(|a| a.kind.type_key()).collect();
        for kind in ["highlight", "note", "freetext", "ink"] {
            assert!(kinds.contains(&kind), "{kind} imported: {kinds:?}");
        }
        let ink = annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Ink { .. }))
            .unwrap();
        assert!(matches!(&ink.kind, AnnoKind::Ink { strokes } if strokes.len() == 2));
        let highlight = annotations
            .iter()
            .find(|a| a.kind.type_key() == "highlight")
            .unwrap();
        assert_eq!(
            highlight.color,
            Swatch::from_rgb(10, 200, 90),
            "exact colour kept"
        );
        let free = annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::FreeText { .. }))
            .unwrap();
        assert!(matches!(&free.kind, AnnoKind::FreeText { text, .. } if text == "See chapter two"));
        let comments = PdfMarkup::of(doc)
            .expect("the PDF markup reads")
            .list_comments();
        let note_id = annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
            .unwrap()
            .id
            .clone();
        let thread = comments
            .iter()
            .find(|c| c.anno_id.as_deref() == Some(note_id.as_str()))
            .expect("the note's thread");
        assert_eq!(thread.body, "Check this");
        assert_eq!(thread.replies.len(), 1, "the reply joined the thread");
        assert_eq!(thread.replies[0].body, "Agreed");
        assert_eq!(thread.replies[0].author, "Bo");
        assert!(
            comments.iter().any(
                |c| c.anno_id.as_deref() == Some(highlight.id.as_str()) && c.body == "Original"
            ),
            "a markup annotation's text became its comment"
        );
        assert!(
            app.reader_markup_hidden,
            "page images leave the taken-over markup to the records"
        );
        app.existing_request = None;
        app.sync(doc, &state.root);
        assert_eq!(
            PdfMarkup::of(doc)
                .expect("the PDF markup reads")
                .list_annotations()
                .len(),
            4
        );

        // Edit: answer the note, rewrite its text, delete the free text; leave
        // the highlight and ink alone.
        app.perform_action(
            &mut Edits::lend(doc),
            crate::actions::add_reply_action_with_id(&thread.id, "reply-new", "Done"),
        );
        app.perform_action(
            &mut Edits::lend(doc),
            crate::actions::set_comment_body_action(&thread.id, "Check this again"),
        );
        app.perform_action(
            &mut Edits::lend(doc),
            crate::actions::delete_annotation_action(&free.id),
        );
        app.reload(doc);

        let out = dir.path().join("exported.pdf");
        assert!(tests::save_settled(&mut app, &out), "{:?}", app.status_note);
        let bytes = std::fs::read(&out).unwrap();
        let engine = tilo_pdf::Engine::shared().expect("engine");
        let reloaded = engine.open_bytes(&bytes).expect("reopen in pdfium");
        let annots = reloaded.annotations(PAGE).unwrap();
        assert_eq!(
            count(&annots, &tilo_pdf::AnnotationKind::Highlight),
            1,
            "{annots:#?}"
        );
        assert_eq!(count(&annots, &tilo_pdf::AnnotationKind::Ink), 1);
        assert_eq!(count(&annots, &tilo_pdf::AnnotationKind::FreeText), 0);
        assert_eq!(count(&annots, &tilo_pdf::AnnotationKind::Text), 3);
        let parent = annots
            .iter()
            .find(|a| a.name == "note-1")
            .expect("the note keeps its name");
        assert_eq!(parent.contents, "Check this again");
        let replies: Vec<&tilo_pdf::AnnotationInfo> = annots
            .iter()
            .filter(|a| a.in_reply_to == Some(parent.index))
            .collect();
        assert_eq!(replies.len(), 2, "both replies thread to the note");
        assert!(
            replies
                .iter()
                .any(|r| r.name == "reply-1" && r.contents == "Agreed")
        );
        assert!(
            replies
                .iter()
                .any(|r| r.author == "test-author" && r.contents == "Done"),
            "a reply is signed by whoever wrote it"
        );
        let original = engine.open_bytes(&std::fs::read(&path).unwrap()).unwrap();
        let before = original.annotations(PAGE).unwrap();
        let hl_before = before.iter().find(|a| a.name == "hl-1").unwrap();
        let hl_after = annots.iter().find(|a| a.name == "hl-1").unwrap();
        assert_eq!(hl_before.quads, hl_after.quads);
        assert_eq!(hl_after.contents, "Original");

        let Some((_again, state2)) = open(&out) else {
            break 'phase;
        };
        let imported = PdfMarkup::of(&state2.doc)
            .expect("the PDF markup reads")
            .list_annotations();
        assert_eq!(imported.len(), 3, "{imported:#?}");
        let threads = PdfMarkup::of(&state2.doc)
            .expect("the PDF markup reads")
            .list_comments();
        let note_thread = threads
            .iter()
            .find(|c| c.body == "Check this again")
            .expect("the note's thread");
        assert_eq!(note_thread.replies.len(), 2);
    }

    eprintln!("phase form_values_in_the_file_become_records_and_only_changes_are_written");
    'phase: {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("form.pdf");
        std::fs::copy(engine_fixture("form.pdf"), &path).expect("copy fixture");
        let Some((mut app, mut state)) = open(&path) else {
            eprintln!("SKIPPING: no PDF engine on this host");
            break 'phase;
        };
        let doc = &mut state.doc;
        let values = PdfMarkup::of(doc)
            .expect("the PDF markup reads")
            .form_values;
        assert_eq!(values.get("name"), Some(&FormEntry::Text("Ada".into())));
        assert_eq!(values.get("colour"), Some(&FormEntry::Choice("Red".into())));
        assert_eq!(
            values.get("agree"),
            None,
            "an unchecked box records nothing"
        );

        app.perform_action(
            &mut Edits::lend(doc),
            crate::actions::set_form_value_action("name", &FormEntry::Text("Grace Hopper".into())),
        );
        app.reload(doc);
        let out = dir.path().join("filled.pdf");
        assert!(tests::save_settled(&mut app, &out), "{:?}", app.status_note);
        let engine = tilo_pdf::Engine::shared().expect("engine");
        let reloaded = engine.open_bytes(&std::fs::read(&out).unwrap()).unwrap();
        let fields = reloaded.form_fields().unwrap();
        let value = |n: &str| fields.iter().find(|f| f.name == n).unwrap().value.clone();
        assert_eq!(value("name"), "Grace Hopper");
        assert_eq!(
            value("colour"),
            "Red",
            "the unchanged value is left as it was"
        );
    }

    eprintln!("phase a_source_is_imported_once_and_concurrent_imports_merge");
    {
        let hash = "ab".repeat(32);
        let mut plan = crate::adopt::ImportPlan::default();
        plan.annotations.push((
            Annotation {
                id: crate::adopt::imported_id(&hash, 0, 3),
                space: crate::model::GeomSpace::User,
                ..Annotation::new(1, AnnoKind::Note { x: 1.0, y: 2.0 }, Swatch::Yellow, "Ada")
            },
            "source-1".into(),
        ));
        let bytes = PdfMarkup::planned(2);
        let mut left = AutoCommit::load(&bytes).unwrap();
        let mut right = AutoCommit::load(&bytes).unwrap();
        let import = || crate::actions::import_existing_action(&hash, &plan);
        PdfMarkup::apply_to(&mut left, import());
        PdfMarkup::apply_to(&mut right, import());
        left.merge_incoming(&mut right).unwrap();
        assert_eq!(
            PdfMarkup::of(&left)
                .expect("the PDF markup reads")
                .list_annotations()
                .len(),
            1,
            "one record, not two"
        );
        assert_eq!(
            PdfMarkup::of(&left)
                .expect("the PDF markup reads")
                .imported_source()
                .map(str::to_owned)
                .as_deref(),
            Some(hash.as_str())
        );
        // Importing the same source again is a no-op, even after a deletion.
        let id = PdfMarkup::of(&left)
            .expect("the PDF markup reads")
            .list_annotations()[0]
            .id
            .clone();
        PdfMarkup::apply_to(&mut left, crate::actions::delete_annotation_action(&id));
        PdfMarkup::apply_to(&mut left, import());
        assert!(
            PdfMarkup::of(&left)
                .expect("the PDF markup reads")
                .list_annotations()
                .is_empty(),
            "a deletion sticks"
        );
        assert_eq!(
            PdfMarkup::of(&left)
                .expect("the PDF markup reads")
                .adopted_entries()
                .len(),
            0,
            "this plan adopted nothing"
        );
    }
}

#[serial_test::serial(pdfium)]
#[test]
fn authored_free_text_and_typed_signatures_export_with_embedded_appearances() {
    eprintln!("phase free_text_is_authored_on_the_page_and_exported_with_an_appearance");
    'phase: {
        if !helper_sets_appearance_text() {
            break 'phase;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("latin.pdf");
        std::fs::copy(engine_fixture("latin.pdf"), &path).expect("copy fixture");
        let Some((mut app, mut state)) = open(&path) else {
            eprintln!("SKIPPING: no PDF engine on this host");
            break 'phase;
        };
        let doc = &mut state.doc;
        app.set_tool(Tool::Text);
        app.create_free_text(&mut Edits::lend(doc), 1, (30.0, 120.0));
        assert!(
            matches!(&app.editing, Some(e) if matches!(e.target, EditTarget::FreeText(_))),
            "a new box opens for typing"
        );
        app.editing
            .as_mut()
            .unwrap()
            .set_text("Überprüfen — 検討してください");
        app.commit_edit(&mut Edits::lend(doc));
        let stored = PdfMarkup::of(doc)
            .expect("the PDF markup reads")
            .list_annotations();
        let free = stored
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::FreeText { .. }))
            .expect("the free text is a record");
        let AnnoKind::FreeText { text, size, .. } = &free.kind else {
            unreachable!()
        };
        assert_eq!(text, "Überprüfen — 検討してください");
        assert!(*size > 0.0);
        // Moving it: drag from inside the box.
        let id = free.id.clone();
        let before = app.note_pos(&id).unwrap();
        app.move_note_local(&id, before.0 + 20.0, before.1 + 10.0);
        app.persist_free_text_box(&mut Edits::lend(doc), &id);
        app.reload(doc);
        let after = app.note_pos(&id).unwrap();
        assert!(
            (after.0 - before.0 - 20.0).abs() < 0.01 && (after.1 - before.1 - 10.0).abs() < 0.01
        );
        app.selected_anno = Some(id.clone());
        app.resize_free_text(&mut Edits::lend(doc), &id, 4.0);

        let out = dir.path().join("with-text.pdf");
        assert!(tests::save_settled(&mut app, &out), "{:?}", app.status_note);
        let bytes = std::fs::read(&out).unwrap();
        let engine = tilo_pdf::Engine::shared().expect("engine");
        let reloaded = engine.open_bytes(&bytes).unwrap();
        let annots = reloaded.annotations(0).unwrap();
        let exported = annots
            .iter()
            .find(|a| a.kind == tilo_pdf::AnnotationKind::FreeText)
            .expect("free text exported");
        assert_eq!(exported.contents, "Überprüfen — 検討してください");
        // pdfium draws it from the generated appearance.
        let scale = 2.0;
        let bitmap = reloaded
            .render(
                0,
                &tilo_pdf::RenderOptions {
                    scale,
                    ..tilo_pdf::RenderOptions::default()
                },
            )
            .unwrap();
        let r = exported.rect;
        let (w, _) = (bitmap.width() as usize, bitmap.height() as usize);
        let mut inked = 0;
        for y in (r.y0 * scale) as usize..(r.y1 * scale) as usize {
            for x in (r.x0 * scale) as usize..(r.x1 * scale) as usize {
                let p = &bitmap.pixels()[(y * w + x) * 4..][..4];
                if p[0] < 200 || p[1] < 200 || p[2] < 200 {
                    inked += 1;
                }
            }
        }
        assert!(inked > 100, "the free text is drawn ({inked} px)");
    }

    eprintln!("phase typed_signatures_in_any_script_are_drawn_with_an_embedded_font");
    'phase: {
        if !helper_sets_appearance_text() {
            break 'phase;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("latin.pdf");
        std::fs::copy(engine_fixture("latin.pdf"), &path).expect("copy fixture");
        let Some((mut app, mut state)) = open(&path) else {
            eprintln!("SKIPPING: no PDF engine on this host");
            break 'phase;
        };
        let doc = &mut state.doc;
        let signature = Signature {
            id: Signature::new_id(),
            page: 1,
            rect: PtRect::new(40.0, 60.0, 200.0, 60.0),
            signer: "محمد 李小龍".into(),
            date: "2026-09-24".into(),
            ink: SignatureInk::Typed,
            space: crate::model::GeomSpace::Page,
        };
        let user = app.page_map(1).signature_to_user(&signature);
        app.perform_action(
            &mut Edits::lend(doc),
            crate::actions::add_signature_on(&user, "source-1"),
        );
        app.reload(doc);
        let out = dir.path().join("signed.pdf");
        assert!(tests::save_settled(&mut app, &out), "{:?}", app.status_note);
        let bytes = std::fs::read(&out).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("/FontFile2") || text.contains("/FontFile3"));
        assert!(!text.contains("/Helvetica"), "no standard font stands in");
        let engine = tilo_pdf::Engine::shared().expect("engine");
        let reloaded = engine.open_bytes(&bytes).unwrap();
        let stamp = reloaded
            .annotations(0)
            .unwrap()
            .into_iter()
            .find(|a| a.kind == tilo_pdf::AnnotationKind::Stamp)
            .expect("the signature is a stamp");
        assert_eq!(stamp.author, "محمد 李小龍");
    }
}
