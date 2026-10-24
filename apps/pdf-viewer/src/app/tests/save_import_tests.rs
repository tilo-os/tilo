//! Saving and importing: markup and page edits survive a save and reopen,
//! a save without a path hands the host the bytes, and imported images and
//! PDFs add pages.

use super::*;

#[serial(pdfium)]
#[test]
fn markup_and_page_edits_persist_across_save_and_reopen() {
    eprintln!("phase save_back_writes_markup_as_real_pdf_annotations");
    'phase: {
        let Some((mut app, mut st)) = open_laid_out_sample() else {
            break 'phase;
        };
        let before = crate::source::open(std::path::Path::new(SAMPLE_PDF), &ContentStore::memory())
            .expect("the sample opens")
            .raster
            .page_annotations(0)
            .len();

        // Place a highlight, a sticky note with a body, and a signature on page
        // 1 — all through the reader's real mutation methods.
        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (70.0, 150.0),
            cur: (360.0, 165.0),
            points: vec![],
        });
        app.commit_drag(&mut Edits::lend(&mut st.doc));
        app.create_note(&mut Edits::lend(&mut st.doc), 1, (120.0, 300.0));
        app.editing.as_mut().unwrap().set_text("Check this claim");
        app.commit_edit(&mut Edits::lend(&mut st.doc));
        app.signer_name = "Grace Hopper".into();
        app.place_signature(&mut Edits::lend(&mut st.doc), 1, (360.0, 520.0));
        assert!(
            app.annotations
                .iter()
                .any(|a| matches!(a.kind, AnnoKind::Markup { .. }))
        );
        assert!(app.signatures.iter().any(|s| s.signer == "Grace Hopper"));

        let out = std::env::temp_dir()
            .join("tilo_pdf_saveback")
            .join("annotated.pdf");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(
            save_settled(&mut app, &out),
            "the save reports success (note: {:?})",
            app.status_note
        );
        assert!(out.exists(), "the annotated .pdf exists on disk");

        // Reopen the saved file: page 1 carries the markup as standard PDF
        // annotations (a highlight, a note holding the comment text, and a
        // stamp for the signature) that any viewer reads back.
        let reopened =
            crate::source::open(&out, &ContentStore::memory()).expect("the saved PDF reopens");
        let annotations = reopened.raster.page_annotations(0);
        assert!(
            annotations.len() >= before + 3,
            "markup became annotations ({before} -> {})",
            annotations.len()
        );
        assert!(
            annotations
                .iter()
                .any(|a| a.kind == tilo_pdf::AnnotationKind::Highlight)
        );
        assert!(annotations.iter().any(|a| {
            a.kind == tilo_pdf::AnnotationKind::Text && a.contents.contains("Check this claim")
        }));
        assert!(annotations.iter().any(|a| {
            a.kind == tilo_pdf::AnnotationKind::Stamp && a.contents.contains("Grace Hopper")
        }));
    }

    eprintln!("phase reorder_persists_across_save_and_reopen");
    'phase: {
        let Some((mut app, mut st)) = open_laid_out_sample() else {
            break 'phase;
        };
        let n = app.doc.page_count();
        assert!(
            n >= 2,
            "the fixture must have at least two pages to test reordering"
        );
        let first = text_layer(&mut app, &mut st, 1).text.trim().to_string();
        let last = text_layer(&mut app, &mut st, n).text.trim().to_string();
        assert_ne!(
            first, last,
            "the two pages carry distinct text so a swap is observable"
        );

        // Move the first page to the end, as the shared rail's Reorder does.
        let ids = app.resource_page_ids.clone();
        edit_pages(
            &mut app,
            &mut st,
            crate::actions::move_page_plan_action(&ids[0], ids.last().map(String::as_str)),
        );
        assert!(app.pages_dirty(), "a reorder flags that a save is needed");
        assert_eq!(app.doc.page_count(), n, "reorder keeps the page count");

        let mut rz = saved_reopen(&mut app, "reordered.pdf");
        assert_eq!(rz.page_count(), n, "the page count persisted");
        assert_eq!(
            rz.page_text(n - 1).text.trim(),
            first,
            "the old first page is now last in the saved-and-reopened file"
        );
    }

    eprintln!("phase delete_page_persists_across_save_and_reopen");
    'phase: {
        let Some((mut app, mut st)) = open_laid_out_sample() else {
            break 'phase;
        };
        let n = app.doc.page_count();
        assert!(n >= 2);
        let second = text_layer(&mut app, &mut st, 2).text.trim().to_string();

        let first_id = app.resource_page_ids[0].clone();
        edit_pages(
            &mut app,
            &mut st,
            crate::actions::delete_page_plan_action(&first_id),
        );
        assert_eq!(app.doc.page_count(), n - 1, "delete drops one page");
        assert!(app.pages_dirty());

        let mut rz = saved_reopen(&mut app, "deleted.pdf");
        assert_eq!(rz.page_count(), n - 1, "the deletion persisted");
        assert_eq!(
            rz.page_text(0).text.trim(),
            second,
            "the former second page is now first"
        );
    }

    eprintln!("phase duplicate_page_persists_across_save_and_reopen");
    'phase: {
        let Some((mut app, mut st)) = open_laid_out_sample() else {
            break 'phase;
        };
        let n = app.doc.page_count();
        let first = text_layer(&mut app, &mut st, 1).text.trim().to_string();

        let entry = PdfMarkup::of(&st.doc).page_plan()[0].clone();
        edit_pages(
            &mut app,
            &mut st,
            crate::actions::duplicate_page_plan_action(&entry),
        );
        assert_eq!(app.doc.page_count(), n + 1, "duplicate adds one page");

        let mut rz = saved_reopen(&mut app, "duplicated.pdf");
        assert_eq!(rz.page_count(), n + 1, "the duplicate persisted");
        assert_eq!(rz.page_text(0).text.trim(), first);
        assert_eq!(
            rz.page_text(1).text.trim(),
            first,
            "page 2 is a copy of page 1"
        );
    }
}

/// A host whose Save keeps the file itself (a browser page's Save dialog
/// or download): it answers a destination with no path and records what
/// is written there. Every other chooser is refused.
struct KeptSaves(std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>);

impl ui::host::FilePicker for KeptSaves {
    fn pick(&self, _: ui::portal::PickRequest) -> ui::portal::PortalChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::PortalOutcome::Failed("no paths here".into()));
        rx
    }
    fn pick_multiple(&self, _: ui::portal::PickRequest) -> ui::portal::MultiChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::MultiOutcome::Failed("no paths here".into()));
        rx
    }
    fn open_file(&self, _: ui::portal::PickRequest) -> ui::portal::OpenFileChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::OpenFileOutcome::Failed("no paths here".into()));
        rx
    }
    fn launch(&self, _: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(ui::portal::LaunchOutcome::Failed("no paths here".into()));
        rx
    }
    fn save_destination(&self, request: ui::portal::PickRequest) -> ui::portal::DestinationChannel {
        let written = std::sync::Arc::clone(&self.0);
        let name = request.suggested_name().unwrap_or("download").to_owned();
        ui::portal::DestinationChannel::answered(ui::portal::DestinationOutcome::Chosen(
            ui::portal::SaveDestination::kept(name, move |bytes| {
                *written.lock().unwrap() = Some(bytes);
                let (tx, rx) = std::sync::mpsc::channel();
                let _ = tx.send(ui::portal::SaveOutcome::Saved);
                rx
            }),
        ))
    }
}

/// Saving where the host keeps the file (a browser page, which has no
/// paths) hands it the flattened PDF instead of failing for want of a
/// path.
#[test]
#[serial(pdfium)]
fn save_to_a_destination_without_a_path_hands_the_host_the_pdf() {
    let Some((mut app, mut st)) = open_laid_out_sample() else {
        return;
    };
    let name = app.suggested_save_name();
    let written = std::sync::Arc::new(std::sync::Mutex::new(None));
    let services = ui::host::HostServices::null().with_files(std::sync::Arc::new(KeptSaves(
        std::sync::Arc::clone(&written),
    )));
    ui::host::scoped(&services, || {
        app.request_save_via_portal();
    });
    settle_until(&mut app, &mut st, |app| {
        app.portal_save_rx.is_none() && app.pending_save.is_none() && app.save_write_rx.is_none()
    });
    let note = app.status_note.as_deref().unwrap_or_default().to_owned();
    let bytes = written
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| panic!("the host received no PDF ({note})"));
    assert!(bytes.starts_with(b"%PDF"), "the saved bytes are a PDF");
    assert_eq!(
        note,
        crate::t_args("pdf-note-saved", &[("name", name.into())]),
        "the note names the copy saved"
    );
    assert!(!note.contains("browser"), "{note}");
}

/// A save that cannot be written names the copy and says what to do next,
/// in the catalogue's words, never the engine's error.
#[test]
#[serial(pdfium)]
fn a_save_that_cannot_be_written_says_what_to_do() {
    let Some((mut app, _st)) = open_laid_out_sample() else {
        return;
    };
    let missing = std::env::temp_dir().join("tilo_pdf_no_such_folder");
    let _ = std::fs::remove_dir_all(&missing);
    let out = missing.join("kept.pdf");
    assert!(
        !save_settled(&mut app, &out),
        "nothing is saved into a folder that is not there"
    );
    assert!(!out.exists());
    assert_eq!(
        app.status_note.as_deref(),
        Some(crate::app::save_failed_note("kept.pdf").as_str()),
        "the note names the copy and what to try"
    );
    assert_eq!(
        crate::app::save_failed_note("kept.pdf"),
        "kept.pdf wasn't saved. Try another folder or name.",
        "the source copy, so a catalogue that lost the message cannot pass as its key"
    );
}

/// Prepare an opened raster sample for page-editing tests: its record
/// seeded and laid out so `current_page` and region rects are valid.
fn open_laid_out_sample() -> Option<(PdfApp, ui::State)> {
    let mut app = open_raster_sample()?;
    let mut st = crate::scenarios::record_state();
    settle_record(&mut app, &mut st);
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.recompute_regions();
    app.sync_viewport_frame();
    Some((app, st))
}

/// Edit the record's page plan with `invoke` and wait for the worker to
/// materialise it.
fn edit_pages(app: &mut PdfApp, st: &mut ui::State, invoke: ui::tilo_resource::Invoke<PdfMarkup>) {
    app.perform_action(&mut Edits::lend(&mut st.doc), invoke);
    settle_until(app, st, |app| app.pending_worker_plan.is_none());
}

fn saved_reopen(app: &mut PdfApp, name: &str) -> crate::source::RasterBackend {
    let out = std::env::temp_dir().join("tilo_pdf_edit").join(name);
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&out);
    assert!(save_settled(app, &out), "save: {:?}", app.status_note);
    assert!(
        !app.pages_dirty(),
        "a successful save clears the page-dirty flag"
    );
    crate::source::open(&out, &ContentStore::memory())
        .expect("the edited PDF reopens")
        .raster
}

/// Wait for an accepted import to reach the record and the worker's
/// page plan.
fn settle_import(app: &mut PdfApp, st: &mut ui::State) {
    settle_until(app, st, |app| {
        app.pending_import.is_none() && app.pending_worker_plan.is_none()
    });
}

#[serial(pdfium)]
#[test]
fn imported_images_and_pdfs_add_pages_and_regions_crop_the_bitmap() {
    eprintln!("phase import_image_adds_a_page_that_survives_save");
    'phase: {
        let Some((mut app, mut st)) = open_laid_out_sample() else {
            break 'phase;
        };
        let n = app.doc.page_count();

        let img = image::RgbaImage::from_pixel(96, 64, image::Rgba([210, 60, 60, 255]));
        let png = std::env::temp_dir()
            .join("tilo_pdf_edit")
            .join("swatch.png");
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        image::DynamicImage::ImageRgba8(img)
            .save(&png)
            .expect("write the test PNG");

        assert!(app.import_file(&png), "import: {:?}", app.status_note);
        settle_import(&mut app, &mut st);
        assert_eq!(app.doc.page_count(), n + 1, "the image import added a page");
        assert!(app.pages_dirty());

        let rz = saved_reopen(&mut app, "with_image.pdf");
        assert_eq!(rz.page_count(), n + 1, "the imported image page persisted");
    }

    eprintln!("phase import_pdf_appends_all_its_pages");
    'phase: {
        let Some((mut app, mut st)) = open_laid_out_sample() else {
            break 'phase;
        };
        let n = app.doc.page_count();
        // Import the sample into itself: page count should exactly double.
        assert!(
            app.import_file(std::path::Path::new(SAMPLE_PDF)),
            "import: {:?}",
            app.status_note
        );
        settle_import(&mut app, &mut st);
        assert_eq!(
            app.doc.page_count(),
            n * 2,
            "every page of the imported PDF was added"
        );

        let rz = saved_reopen(&mut app, "merged.pdf");
        assert_eq!(rz.page_count(), n * 2, "the merged page set persisted");
    }

    eprintln!("phase copy_image_region_crops_the_page_bitmap");
    'phase: {
        let Some((mut app, mut st)) = open_laid_out_sample() else {
            break 'phase;
        };
        let (pw, ph) = app.raster_document.as_ref().unwrap().page_sizes[0];

        // Marquee the upper-left portion of page 1 and copy it.
        app.region = Some(RegionSelect {
            page: 1,
            start: (40.0, 40.0),
            cur: (pw * 0.6, ph * 0.5),
        });
        // Releasing the marquee asks for the page rendered for the crop,
        // and the marquee itself is gone: the copy follows on its own once
        // the worker delivers the render, with nothing more from the person.
        let bucket = ((pw * 2.0).round() as u32).clamp(96, 4096);
        let key = (0, crate::source::RasterBackend::bucket_for(bucket));
        assert!(
            !app.raster_images.contains_key(&key),
            "the crop's render is not here before the release"
        );
        assert!(!app.release_region(), "the crop waits for its page render");
        assert!(app.region.is_none(), "the release ends the marquee");
        assert!(
            app.status_note.is_progress(),
            "the wait reads as work under way: {:?}",
            app.status_note
        );
        settle_until(&mut app, &mut st, |app| {
            app.raster_images.contains_key(&key) && app.region_waiting.is_none()
        });
        let note = app.status_note.as_deref().unwrap_or_default().to_owned();
        assert!(
            [
                crate::t("pdf-note-copied-area"),
                crate::t("pdf-note-area-not-copied")
            ]
            .contains(&note),
            "the arrived render is cropped and copied, and the note says so: {note:?}"
        );
        assert!(!app.status_note.is_progress(), "{note:?}");

        let img = app.raster_images.get(&key).expect("the render arrived");
        let rect = PtRect::from_corners(40.0, 40.0, pw * 0.6, ph * 0.5);
        let (w, h, rgba) = render::crop_page_region(&img, pw, ph, rect).expect("crop succeeds");
        assert!(w > 0 && h > 0);
        assert_eq!(
            rgba.len(),
            w * h * 4,
            "RGBA buffer matches the cropped dimensions"
        );
        assert!(
            w < img.width as usize && h < img.height as usize,
            "the crop is a proper sub-region of the full page bitmap"
        );
    }
}

/// Release a marquee over the upper-left of displayed page `page` before
/// its page is rendered for the crop: the copy waits for that render.
/// The bitmap key the crop is cut from.
fn release_region_awaiting_render(app: &mut PdfApp, page: usize) -> (usize, u32) {
    let (pw, ph) = app.raster_document.as_ref().unwrap().page_sizes[0];
    let region = RegionSelect {
        page,
        start: (40.0, 40.0),
        cur: (pw * 0.6, ph * 0.5),
    };
    let key = app.region_source(region).0;
    assert!(
        !app.raster_images.contains_key(&key),
        "the crop's render is not here before the release"
    );
    app.region = Some(region);
    assert!(!app.release_region(), "the crop waits for its page render");
    assert!(app.region_waiting.is_some(), "the copy waits");
    assert!(app.status_note.is_progress(), "{:?}", app.status_note);
    key
}

/// A released marquee whose render result was lost before the reader saw
/// it (the bounded result queue lets its oldest results go) asks for the
/// render again and is copied, instead of saying it is copying for good.
#[serial(pdfium)]
#[test]
fn a_region_copy_whose_render_result_was_lost_asks_again() {
    let Some((mut app, mut st)) = open_laid_out_sample() else {
        return;
    };
    let key = release_region_awaiting_render(&mut app, 1);
    let handle = app
        .raster_handle
        .as_ref()
        .expect("the sample opened on a worker");
    let deadline = tilo_test_env::hang_deadline();
    // The render finishes, and its result goes before the reader drains it.
    while !handle.drain_events().iter().any(|event| {
        matches!(
            event,
            crate::source::RasterEvent::Rendered { request, .. }
                if (request.page, crate::source::RasterBackend::bucket_for(request.target_width)) == key
        )
    }) {
        assert!(ui::time::now() < deadline, "the crop's render never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    handle.note_results_evicted();
    settle_until(&mut app, &mut st, |app| app.region_waiting.is_none());
    assert!(
        app.raster_images.contains_key(&key),
        "the render was asked for again and arrived"
    );
    let note = app.status_note.as_deref().unwrap_or_default().to_owned();
    assert!(
        [
            crate::t("pdf-note-copied-area"),
            crate::t("pdf-note-area-not-copied")
        ]
        .contains(&note),
        "the arrived render is cropped, and the note says how the copy went: {note:?}"
    );
    assert!(!app.status_note.is_progress(), "{note:?}");
}

/// A released marquee whose page the engine draws no bitmap for is not
/// copied, and the reader says so instead of saying it is copying for good.
#[serial(pdfium)]
#[test]
fn a_region_copy_whose_page_cannot_be_drawn_says_it_was_not_copied() {
    let Some((mut app, mut st)) = open_laid_out_sample() else {
        return;
    };
    // A page past the document's last: the engine answers with no bitmap.
    let past_last = app.raster_document.as_ref().unwrap().page_sizes.len() + 1;
    let key = release_region_awaiting_render(&mut app, past_last);
    settle_until(&mut app, &mut st, |app| app.region_waiting.is_none());
    assert!(!app.raster_images.contains_key(&key), "no bitmap was drawn");
    assert_eq!(
        app.status_note.as_deref(),
        Some(crate::t("pdf-note-area-not-copied").as_str()),
        "the person is told the area was not copied"
    );
    assert!(!app.status_note.is_progress());
}

/// Saving a copy before any PDF is open (an agent's Save As on an empty
/// window) says to open one first, not to try another folder or name.
#[test]
fn saving_with_no_pdf_open_says_to_open_one_first() {
    let mut app = PdfApp::empty();
    let out = std::env::temp_dir().join("tilo_pdf_nothing_open.pdf");
    assert!(!app.save_annotated_to(&out), "there is nothing to save");
    assert!(!out.exists());
    assert_eq!(
        app.status_note.as_deref(),
        Some(crate::t("pdf-note-save-no-document").as_str()),
    );
}
