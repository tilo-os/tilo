//! Presentation files and operations from outside the editor: opening and
//! saving through the host's chooser and the I/O worker, failures and
//! cancellation, reveals the palette or an agent asks for, and the editor
//! following a restored or collaborator-changed deck.

use super::bench::{Bench, deck_of};
use super::*;

/// Run frames until the editor's open or save job settled.
fn wait_for_document_job(bench: &mut Bench) {
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        bench.frame();
        let settled = bench
            .read(|app| app.active_document_generation.is_none() && app.dispatched_save.is_none());
        if settled {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "background presentation operation did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn write_deck(path: &std::path::Path, deck: &Deck) {
    let bytes = if path.extension().is_some_and(|ext| ext == "pptx") {
        crate::pptx::write_pptx(deck, &ContentStore::memory()).unwrap()
    } else {
        crate::odp::write_odp(deck, &ContentStore::memory()).unwrap()
    };
    std::fs::write(path, bytes).unwrap();
}

/// Open the file the host's chooser answers with, as Ctrl+O does.
fn open_through_the_chooser(bench: &mut Bench, chosen: PortalOutcome) -> Vec<PickRequest> {
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick(chosen);
    ui::host::scoped(&services, || {
        bench
            .app
            .harness()
            .with_widget_mut::<SlidesApp, _>(SlidesApp::request_odp_open)
    });
    wait_for_document_job(bench);
    bench.frame();
    recorder.pick_requests()
}

/// Invoke the deck's operation `id` (`open`, `save-as`, ...) with
/// `arguments`, as the palette or an agent does.
fn operate(
    bench: &mut Bench,
    resource: &str,
    id: &str,
    arguments: &serde_json::Value,
) -> Result<ui::widget::ResourceOperationStart, tilo_actions::DoorError> {
    let instance_id = bench.app.harness().component_instance_id().unwrap();
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id,
        address: tilo_actions::ActionAddress::new(resource, id),
    };
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    bench
        .app
        .harness()
        .invoke_bound_action_confirmed(&address, &arguments)
}

/// ODP and `PowerPoint` files open through the chooser and the worker, as one
/// undoable change; failures, stale results and lost workers are visible.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn document_open_save_and_worker_outcomes() {
    let directory = tempfile::tempdir().unwrap();

    // Case `a_saved_odp_opens_as_the_same_deck_in_one_undo_step`.
    {
        let deck = crate::model::seed_deck();
        let path = directory.path().join("roundtrip.odp");
        save_presentation_to_path(&deck, &path, &ContentStore::memory()).unwrap();
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let before = bench.deck();
        let requests = open_through_the_chooser(&mut bench, PortalOutcome::Opened(path));
        assert_eq!(requests[0].title(), tr("slides-open-title"));
        let opened = bench.deck();
        assert_eq!(
            opened.title, deck.title,
            "open(save(deck)) is the same deck"
        );
        assert_eq!(opened.slides.len(), deck.slides.len());
        assert_eq!(
            bench.read(|app| app.deck.clone()),
            opened,
            "the editor shows it"
        );
        assert_eq!(
            bench.read(|app| app.cur_slide),
            0,
            "it opens on its first slide"
        );
        bench.read(|app| {
            assert!(
                app.doc_portal_rx.is_none(),
                "a resolved request clears the channel"
            );
            assert!(matches!(
                app.io_toast.as_ref(),
                Some((_, ToastKind::Success))
            ));
        });
        assert!(bench.undo(), "opening a file is one undo step");
        assert_eq!(bench.deck(), before, "undo puts the previous deck back");
    }

    // Case `app_routes_powerpoint`.
    {
        let deck = crate::model::seed_deck();
        let path = directory.path().join("roundtrip.pptx");
        write_deck(&path, &deck);
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        open_through_the_chooser(&mut bench, PortalOutcome::Opened(path));
        let opened = bench.deck();
        assert_eq!(opened.title, deck.title);
        assert_eq!(opened.slides.len(), deck.slides.len());
        assert!(bench.read(|app| matches!(
            app.io_toast.as_ref(),
            Some((message, ToastKind::Success)) if *message == tr("slides-toast-opened")
        )));
    }

    // Case `failed_open_is_visible_instead_of_silent`.
    {
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let before = bench.deck();
        open_through_the_chooser(
            &mut bench,
            PortalOutcome::Opened(directory.path().join("missing.odp")),
        );
        assert!(bench.read(|app| matches!(app.io_toast.as_ref(), Some((_, ToastKind::Error)))));
        assert_eq!(bench.deck(), before, "a failed open changes nothing");
    }

    // Case `cancelled_document_job_cannot_apply_a_stale_open_result`.
    {
        let mut imported = crate::model::seed_deck();
        imported.title = "stale import".into();
        let path = directory.path().join("stale.odp");
        write_deck(&path, &imported);
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let original = bench.deck().title;
        bench.with(|app| {
            assert!(app.queue_document_open(path.clone()));
            // What `poll_resource_operation` does when the caller cancels.
            app.active_document_generation = None;
        });
        for _ in 0..50 {
            bench.frame();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            bench.deck().title,
            original,
            "a stale parser completion is inert"
        );
    }

    // Case `disconnected_document_worker_settles_the_accepted_resource_operation`.
    Bench::one_slide(Slide::new(Layout::Blank)).with(|app| {
        // A sender for future jobs, but a result channel whose worker is gone.
        let (tx, _jobs) = std::sync::mpsc::sync_channel(1);
        let (results, rx) = std::sync::mpsc::sync_channel(1);
        drop(results);
        app.document_worker = Some(DocumentWorker { tx, rx });
        app.resource_invocation = Some(tilo_actions::InvocationId::new());
        app.active_document_generation = Some(7);
        app.drain_document_worker();
        assert!(app.document_worker.is_none());
        assert!(app.active_document_generation.is_none());
        assert!(app.resource_invocation.is_none());
        assert!(matches!(
            app.completed_resource_invocation,
            Some(tilo_actions::InvocationUpdate::Failed { .. })
        ));
        assert!(matches!(app.io_toast.as_ref(), Some((_, ToastKind::Error))));
    });

    // Case `a_save_writes_the_deck_and_settles`.
    {
        let path = directory.path().join("saved.pptx");
        let mut bench = Bench::sample();
        let queued = bench.with(|app| {
            app.resource_invocation = Some(tilo_actions::InvocationId::new());
            app.queue_document_save(path.clone())
        });
        assert!(queued);
        wait_for_document_job(&mut bench);
        assert!(bench.read(|app| matches!(app.io_toast.as_ref(), Some((_, ToastKind::Success)))));
        let saved = crate::pptx::read_pptx(&std::fs::read(&path).unwrap(), &ContentStore::memory())
            .unwrap();
        assert_eq!(saved.title, bench.deck().title);
    }

    // Case `ensure_odp_ext_appends_when_missing`.
    {
        assert_eq!(
            ensure_odp_ext(std::path::Path::new("/x/talk"))
                .to_str()
                .unwrap(),
            "/x/talk.odp"
        );
        assert_eq!(
            ensure_odp_ext(std::path::Path::new("/x/talk.odp"))
                .to_str()
                .unwrap(),
            "/x/talk.odp"
        );
    }
}

/// The Open operation with a path opens that file through the worker and
/// answers its caller once the deck is replaced; Save As writes the deck.
#[test]
fn open_and_save_as_operations_reach_the_files_named() {
    let directory = tempfile::tempdir().unwrap();
    let mut deck = crate::model::seed_deck();
    deck.title = "Named on the call".into();
    let path = directory.path().join("named.odp");
    write_deck(&path, &deck);

    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    let started = operate(
        &mut bench,
        "slides.deck",
        "open",
        &serde_json::json!({ "path": path.to_string_lossy() }),
    )
    .expect("open is accepted");
    assert!(matches!(
        started,
        ui::widget::ResourceOperationStart::Accepted
    ));
    wait_for_document_job(&mut bench);
    bench.frame();
    assert_eq!(bench.deck().title, "Named on the call");

    let out = directory.path().join("written.odp");
    operate(
        &mut bench,
        "slides.deck",
        "save-as",
        &serde_json::json!({ "path": out.to_string_lossy() }),
    )
    .expect("save-as is accepted");
    wait_for_document_job(&mut bench);
    let written =
        crate::odp::read_odp(&std::fs::read(&out).unwrap(), &ContentStore::memory()).unwrap();
    assert_eq!(written.title, "Named on the call");
}

/// A reveal from the palette or an agent shows the exact slide and element,
/// its words selected for editing.
#[test]
fn a_brokered_text_reveal_restores_the_slide_element_and_selection() {
    let mut bench = Bench::sample();
    let slide_index = 2;
    let (slide_id, element_id, text_len) = {
        let deck = bench.deck();
        let slide = &deck.slides[slide_index];
        let element = slide
            .elements
            .iter()
            .find(|element| element.kind == ElementKind::Text && !element.text.is_empty())
            .unwrap();
        (
            slide.id.clone(),
            element.id.clone(),
            element.text.chars().count(),
        )
    };
    let _ = slide_id;
    operate(
        &mut bench,
        &crate::actions::text_resource_id(&element_id),
        "open",
        &serde_json::json!({}),
    )
    .expect("the text resource reveals");
    bench.frame();
    bench.read(|app| {
        assert_eq!(app.cur_slide, slide_index);
        assert_eq!(app.sel.as_deref(), Some(element_id.as_str()));
        assert!(app.editing_text);
        assert_eq!(app.mode, Mode::Edit);
        let editor = app
            .text_editor
            .as_ref()
            .expect("the text box is being edited")
            .host
            .widget()
            .editor();
        assert_eq!(
            editor.selected_text().unwrap_or_default().chars().count(),
            text_len,
            "the revealed text is selected in full"
        );
    });
}

/// A restore while the notes are open shows the restored notes, and typing
/// goes on in them.
#[test]
fn a_restore_rebinds_the_notes_being_edited() {
    let mut s = Slide::new(Layout::Blank);
    s.notes = "Selected notes".into();
    let mut bench = Bench::one_slide(s);
    let selected = bench.app.read_document(|doc| doc.clone().get_heads());
    bench
        .app
        .invoke_child::<ui::tilo_resource::PlainText>(
            &crate::actions::notes_resource_id(&bench.deck().slides[0].id),
            ui::tilo_resource::Invoke::apply(&ui::tilo_resource::TextEdit::Splice {
                at: 0,
                delete: 0,
                insert: "LIVE ".into(),
            })
            .unwrap(),
        )
        .unwrap();
    bench.focus();
    bench.with(SlidesApp::begin_notes_edit);
    bench.frame();
    assert_eq!(
        bench.read(|app| app.notes.control().value().to_owned()),
        "LIVE Selected notes"
    );
    // History's restore rewrites the deck as it was.
    bench.app.merge_peer_edit(|doc| {
        let past = doc.fork_at(&selected).unwrap();
        let deck = SlidesDoc::load(&past).unwrap();
        tilo_actions::Action::apply(&actions::replace_deck_action(&deck), doc).unwrap();
    });
    bench.frame();
    bench.read(|app| {
        assert!(app.editing_notes);
        assert_eq!(app.notes.control().value(), "Selected notes");
    });
    bench.named(NamedKey::End);
    bench.type_text("!");
    assert_eq!(bench.deck().slides[0].notes, "Selected notes!");
}

/// When the slide whose notes are open goes away, the notes editor closes
/// rather than typing into another slide's notes.
#[test]
fn notes_of_a_deleted_slide_stop_being_edited() {
    let mut first = Slide::new(Layout::Blank);
    first.notes = "first".into();
    let mut second = Slide::new(Layout::Blank);
    second.notes = "second".into();
    let gone = second.id.clone();
    let mut bench = Bench::open(&deck_of(vec![first, second]), super::bench::WINDOW);
    bench.with(|app| {
        app.cur_slide = 1;
        app.begin_notes_edit();
    });
    bench.frame();
    assert!(bench.read(|app| app.editing_notes));
    bench.app.merge_peer_edit(|doc| {
        tilo_actions::Action::apply(&actions::delete_slide_action(1, gone.clone()), doc).unwrap();
    });
    bench.frame();
    bench.read(|app| {
        assert!(!app.editing_notes, "the notes of a deleted slide close");
        assert!(app.deck.index_of(&gone).is_none());
    });
}

/// A collaborator's slide inserted before the one shown leaves it shown.
#[test]
fn the_slide_shown_stays_shown_when_a_collaborator_inserts_before_it() {
    let mut bench = Bench::sample();
    bench.with(|app| app.cur_slide = 2);
    let shown = bench.deck().slides[2].id.clone();
    bench.app.merge_peer_edit(|doc| {
        let ranks = SlidesDoc::slide_ranks(doc);
        let slide = Slide::new(Layout::Blank);
        tilo_actions::Action::apply(
            &actions::insert_slide_with_order_action(
                &slide,
                crate::doc::rank_for_insert(&ranks, 0),
            ),
            doc,
        )
        .unwrap();
    });
    bench.frame();
    bench.read(|app| {
        assert_eq!(app.slide().map(|slide| slide.id.clone()), Some(shown));
        assert_eq!(app.cur_slide, 3);
    });
}
