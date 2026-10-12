//! Presentation files and effects from outside the editor: opening and
//! saving through the host's chooser or a path, failures, reveals the
//! palette or an agent asks for, and the editor following a restored or
//! collaborator-changed deck.

use super::bench::{Bench, deck_of};
use super::*;
use crate::effects::SlidesEffect;
use ui::portal::PortalOutcome;

fn write_deck(path: &std::path::Path, deck: &Deck) {
    let bytes = if path.extension().is_some_and(|ext| ext == "pptx") {
        crate::pptx::write_pptx(deck, &ContentStore::memory()).unwrap()
    } else {
        crate::odp::write_odp(deck, &ContentStore::memory()).unwrap()
    };
    std::fs::write(path, bytes).unwrap();
}

/// Run `effect` with the host's chooser answering `chosen`, as Ctrl+O and
/// Ctrl+S do, confirmed where it asks; the chooser's requests and the
/// effect's answer.
fn through_the_chooser(
    bench: &mut Bench,
    chosen: PortalOutcome,
    effect: &SlidesEffect,
) -> (
    Vec<PickRequest>,
    Result<tilo_actions::ActionOutput, tilo_actions::DoorError>,
) {
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick(chosen);
    let answer = ui::host::scoped(&services, || bench.app.effect_confirmed(effect));
    bench.frame();
    (recorder.pick_requests(), answer)
}

const OPEN: SlidesEffect = SlidesEffect::Open { path: None };

/// ODP and `PowerPoint` files open through the chooser as one undoable
/// change; failures change nothing and are said.
#[test]
fn presentation_files_open_and_save_through_the_chooser() {
    let directory = tempfile::tempdir().unwrap();

    // Case `a_saved_odp_opens_as_the_same_deck_in_one_undo_step`.
    {
        let deck = crate::model::seed_deck();
        let path = directory.path().join("roundtrip.odp");
        save_presentation_to_path(&deck, &path, &ContentStore::memory()).unwrap();
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let before = bench.deck();
        let (requests, answer) =
            through_the_chooser(&mut bench, PortalOutcome::Opened(path), &OPEN);
        answer.expect("the file opens");
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
        bench.read(|app| {
            assert_eq!(app.cur_slide, 0, "it opens on its first slide");
            assert!(matches!(
                app.io_toast.as_ref(),
                Some((message, ToastKind::Success)) if *message == tr("slides-toast-opened")
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
        through_the_chooser(&mut bench, PortalOutcome::Opened(path), &OPEN)
            .1
            .expect("the PowerPoint file opens");
        let opened = bench.deck();
        assert_eq!(opened.title, deck.title);
        assert_eq!(opened.slides.len(), deck.slides.len());
    }

    // Case `a_cancelled_choice_opens_nothing`.
    {
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let before = bench.deck();
        through_the_chooser(&mut bench, PortalOutcome::Cancelled, &OPEN)
            .1
            .expect("a cancel is no failure");
        assert_eq!(bench.deck(), before);
    }

    // Case `a_file_that_is_no_presentation_is_said_and_changes_nothing`.
    {
        let path = directory.path().join("broken.odp");
        std::fs::write(&path, b"not a zip").unwrap();
        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        let before = bench.deck();
        let (_, answer) = through_the_chooser(&mut bench, PortalOutcome::Opened(path), &OPEN);
        assert!(answer.is_err(), "the caller hears the open failed");
        assert!(bench.read(|app| matches!(app.io_toast.as_ref(), Some((_, ToastKind::Error)))));
        assert_eq!(bench.deck(), before, "a failed open changes nothing");
    }

    // Case `saving_through_the_chooser_writes_the_deck_as_odp`.
    {
        let out = directory.path().join("chosen.odp");
        let mut bench = Bench::sample();
        through_the_chooser(
            &mut bench,
            PortalOutcome::Opened(out.clone()),
            &SlidesEffect::SaveAs { path: None },
        )
        .1
        .expect("the deck saves");
        let saved =
            crate::odp::read_odp(&std::fs::read(&out).unwrap(), &ContentStore::memory()).unwrap();
        assert_eq!(saved.title, bench.deck().title);
        assert!(bench.read(|app| matches!(app.io_toast.as_ref(), Some((_, ToastKind::Success)))));
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

/// Open and Save As with a path reach the files named, as an agent or a
/// script asks; Read answers with the deck as text.
#[test]
fn open_save_as_and_read_reach_the_files_named() {
    let directory = tempfile::tempdir().unwrap();
    let mut deck = crate::model::seed_deck();
    deck.title = "Named on the call".into();
    let path = directory.path().join("named.odp");
    write_deck(&path, &deck);

    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    bench
        .app
        .effect_confirmed(&SlidesEffect::Open {
            path: Some(path.to_string_lossy().into_owned()),
        })
        .expect("open is answered");
    bench.frame();
    assert_eq!(bench.deck().title, "Named on the call");

    let out = directory.path().join("written.pptx");
    bench
        .app
        .effect(&SlidesEffect::SaveAs {
            path: Some(out.to_string_lossy().into_owned()),
        })
        .expect("save-as is answered");
    let written =
        crate::pptx::read_pptx(&std::fs::read(&out).unwrap(), &ContentStore::memory()).unwrap();
    assert_eq!(
        written.title, "Named on the call",
        "a .pptx path writes PowerPoint"
    );

    let read = bench.app.effect(&SlidesEffect::Read).expect("read answers");
    let outline = read
        .value
        .and_then(|value| value.as_str().map(str::to_owned));
    assert!(
        outline.is_some_and(|outline| outline.starts_with("Named on the call")),
        "read answers with the deck's outline"
    );

    // Opening replaces the shared deck for everyone: it asks first.
    assert!(
        bench
            .app
            .effect(&SlidesEffect::Open {
                path: Some(path.to_string_lossy().into_owned()),
            })
            .is_err(),
        "an unconfirmed open is held for confirmation"
    );
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
    bench
        .app
        .effect_on(
            &crate::actions::text_resource_id(&element_id),
            &SlidesEffect::EditText {
                slide: slide_id,
                element: element_id.clone(),
            },
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

/// An agent with no files of its own places the picture an export answered
/// by its content address, then exports the deck as `.pptx` and a slide
/// as a PNG kept as content: the `.pptx` carries the picture, the PNG is the
/// slide at 1280×720, and neither asks for a chooser.
#[test]
fn a_picture_by_content_address_travels_into_the_exports_kept_as_content() {
    use image::ImageEncoder as _;
    let mut picture = Vec::new();
    image::codecs::png::PngEncoder::new(&mut picture)
        .write_image(&[40; 64], 4, 4, image::ExtendedColorType::Rgba8)
        .unwrap();
    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    let content = bench.app.content();
    let hash = content.put(&picture).unwrap();
    let slide = bench.deck().slides[0].id.clone();
    let outline = bench
        .app
        .call(
            actions::ADD_ELEMENT,
            serde_json::json!({"slide": slide, "kind": "image", "src": format!("tilo.content:{hash}"),
                               "x": 100, "y": 100, "w": 400, "h": 300}),
        )
        .expect("the picture is placed")
        .and_then(|value| value.as_str().map(str::to_owned))
        .expect("add-element answers the outline");
    let element = bench.deck().slides[0].elements[0].clone();
    assert_eq!(element.src, format!("asset:blake3:{hash}"));
    assert!(
        outline.contains(&format!("[image] {{#{}}}", element.id)),
        "the outline names the new element: {outline}"
    );

    let kept = |bench: &mut Bench, effect: &SlidesEffect| {
        let (services, recorder) = ui::host::HostServices::recording();
        let output = ui::host::scoped(&services, || bench.app.effect(effect)).expect("exported");
        assert!(recorder.pick_requests().is_empty(), "no chooser is asked");
        let [address] = output.resources.as_slice() else {
            panic!("one content resource: {output:?}");
        };
        let hash = tilo_actions::content_hash(address).expect("a content address");
        bench.app.content().read(hash).expect("the bytes are kept")
    };

    let pptx = kept(&mut bench, &SlidesEffect::ExportPptxContent);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&pptx)).expect("a .pptx package");
    let mut media = Vec::new();
    std::io::Read::read_to_end(
        &mut zip
            .by_name("ppt/media/image1.png")
            .expect("the picture is inside"),
        &mut media,
    )
    .unwrap();
    assert_eq!(media, picture);
    let reopened = crate::pptx::read_pptx(&pptx, &ContentStore::memory()).unwrap();
    assert_eq!(reopened.slides[0].elements[0].kind, ElementKind::Image);

    let png = kept(
        &mut bench,
        &SlidesEffect::ExportSlidePngContent { slide: 1 },
    );
    let drawn = image::load_from_memory(&png).expect("a PNG");
    assert_eq!((drawn.width(), drawn.height()), (1280, 720));
    assert!(
        bench
            .app
            .effect(&SlidesEffect::ExportSlidePngContent { slide: 2 })
            .is_err(),
        "a slide the deck lacks is refused"
    );
}
