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

const OPEN: SlidesEffect = SlidesEffect::OpenPresentation { path: None };

/// Foreign producers may wrap words in CDATA instead of escaping them.
/// Opening those packages must keep the words and their surrounding marks.
#[test]
fn opening_presentations_preserves_cdata_text() {
    use std::io::{Cursor, Read, Write};

    const WORDS: &str = "Before <draft> &amp; café after";
    for extension in ["odp", "pptx"] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("cdata.{extension}"));
        let mut slide = Slide::new(Layout::Blank);
        slide.add(
            Element::text("Before CDATA_WORDS after", 20.0, 30.0, 600.0, 100.0, 32.0).with(
                |element| {
                    element.bold = true;
                    element.italic = true;
                },
            ),
        );
        slide.notes = "Before CDATA_NOTES after".into();
        let mut original = deck_of(vec![slide]);
        original.title = "Before CDATA_TITLE after".into();
        write_deck(&path, &original);

        let bytes = std::fs::read(&path).unwrap();
        let mut source = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut output = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for index in 0..source.len() {
            let mut entry = source.by_index(index).unwrap();
            let name = entry.name().unwrap().into_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if name.ends_with(".xml") {
                let xml = String::from_utf8(bytes).unwrap();
                // The exact sentinel is text content, never an attribute.
                bytes = xml
                    .replace(
                        ">Before CDATA_WORDS after<",
                        ">Before <![CDATA[<draft> &amp; café]]> after<",
                    )
                    .replace(
                        ">Before CDATA_NOTES after<",
                        ">Before <![CDATA[<draft> &amp; café]]> after<",
                    )
                    .replace(
                        ">Before CDATA_TITLE after<",
                        ">Before <![CDATA[<draft> &amp; café]]> after<",
                    )
                    .into_bytes();
            }
            output
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            output.write_all(&bytes).unwrap();
        }
        std::fs::write(&path, output.finish().unwrap().into_inner()).unwrap();

        let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
        through_the_chooser(&mut bench, PortalOutcome::Opened(path), &OPEN)
            .1
            .expect("the presentation opens");
        let opened = bench.deck();
        let element = &opened.slides[0].elements[0];
        assert_eq!(
            element.text, WORDS,
            "{extension} keeps literal CDATA, not decoded entities"
        );
        let blocks = element.rich_blocks();
        let runs = crate::model::runs_of(&blocks[0]);
        assert!(!runs.is_empty(), "the imported text has styled runs");
        assert!(runs.iter().all(|(_, bold, italic)| *bold && *italic));
        if extension == "pptx" {
            assert_eq!(opened.title, WORDS);
            assert_eq!(opened.slides[0].notes, WORDS);
        }
    }
}

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
            assert!(
                !matches!(app.io_toast.get(), Some((_, ToastKind::Success))),
                "the opened deck on screen is the confirmation; no toast repeats it"
            );
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
        assert!(bench.read(|app| matches!(app.io_toast.get(), Some((_, ToastKind::Error)))));
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
        assert!(
            !bench.read(|app| matches!(app.io_toast.get(), Some((_, ToastKind::Success)))),
            "the person chose the file; no toast repeats the save"
        );
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
        .effect_confirmed(&SlidesEffect::OpenPresentation {
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

    let read = bench
        .app
        .effect(&SlidesEffect::ReadDeck)
        .expect("read answers");
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
            .effect(&SlidesEffect::OpenPresentation {
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
        .effect(&SlidesEffect::EditText {
            slide: slide_id,
            element: element_id.clone(),
        })
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
    let notes = crate::actions::slide_paths(&bench.deck().slides[0].id)
        .notes
        .splice(0, 0, "LIVE ");
    bench
        .app
        .invoke(ui::tilo_resource::Invoke::edit(&notes).unwrap())
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
        let deck = <crate::deck::Deck as ui::tilo_resource::Resource>::read(&past).unwrap();
        tilo_actions::Action::apply(
            &actions::replace_deck_action(&crate::deck::mirror(&deck)),
            doc,
        )
        .unwrap();
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
        tilo_actions::Action::apply(&actions::delete_slide_action(&gone), doc).unwrap();
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
        let slide = Slide::new(Layout::Blank);
        tilo_actions::Action::apply(&actions::insert_slide_action(0, &slide), doc).unwrap();
    });
    bench.frame();
    bench.read(|app| {
        assert_eq!(app.slide().map(|slide| slide.id.clone()), Some(shown));
        assert_eq!(app.cur_slide, 3);
    });
}
