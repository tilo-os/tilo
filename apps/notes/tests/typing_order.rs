//! Letters typed into a note land in the order they were typed, whichever
//! way they arrive: keys with frames in between, or input-method commits.

use notes::actions::create_page_action;
use notes::read;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use ui::winit::event::{Ime, WindowEvent};

fn with_focused_page() -> (TestApp<NotesShell>, PageId) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&create_page_action(page, None, "Order")).unwrap())
        .unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .unwrap();
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    (app, page)
}

fn body(app: &TestApp<NotesShell>, page: PageId) -> String {
    app.read_document(|doc| {
        read::page_block_snapshots(doc, page)
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("\n")
    })
}

#[test]
fn typed_letters_land_in_the_order_they_were_typed() {
    let (mut app, page) = with_focused_page();
    app.harness().type_text("hi");
    assert_eq!(body(&app, page), "hi");
}

#[test]
fn a_sentence_typed_with_frames_between_keys_reads_back_exactly() {
    let (mut app, page) = with_focused_page();
    let sentence = "The quick brown fox jumps over the lazy dog, twice!";
    for ch in sentence.chars() {
        app.harness().key(&ch.to_string());
        let _ = app.harness().poll_frame();
    }
    assert_eq!(body(&app, page), sentence);
}

#[test]
fn input_method_commits_land_in_order() {
    let (mut app, page) = with_focused_page();
    for text in ["h", "i", " ", "é", "👍🏽"] {
        let harness = app.harness();
        harness.send(&WindowEvent::Ime(Ime::Preedit(text.into(), Some((0, 0)))));
        harness.send(&WindowEvent::Ime(Ime::Preedit(String::new(), None)));
        harness.send(&WindowEvent::Ime(Ime::Commit(text.into())));
    }
    assert_eq!(body(&app, page), "hi é👍🏽");
}

#[test]
fn physical_observer_matches_real_note_dispatch_to_the_drawn_body_without_text_logs() {
    let (mut app, page) = with_focused_page();
    let mut trace = ui::NativePhysicalInputTrace::default();
    let before = app
        .harness()
        .native_input_snapshot()
        .expect("Notes observer");
    let event = WindowEvent::Ime(Ime::Commit("é".into()));
    let input = trace.begin(&event, Some(before)).expect("input identity");
    app.harness().send(&event);
    let effect = ui::NativePhysicalInputTrace::effect(input, app.harness().native_input_snapshot());
    let _ = app.harness().poll_frame();
    let frame = trace
        .drawn(Some(71), app.harness().native_input_snapshot())
        .expect("drawn frame");
    assert_eq!(body(&app, page), "é");
    assert_eq!(effect["observer_session"], frame["observer_session"]);
    uuid::Uuid::parse_str(
        frame["observer_session"]
            .as_str()
            .expect("app worker lifetime identity"),
    )
    .expect("random lifetime UUID, independent of the namespace PID");
    assert_eq!(frame["input_ids"], serde_json::json!([1]));
    assert_eq!(frame["cpu_frame_id"], 71);
    assert_eq!(frame["state"]["body_chars"], 1);
    assert_eq!(frame["state"]["body_bytes"], 2);
    assert_eq!(
        frame["state"]["body_hash"],
        blake3::hash("é".as_bytes()).to_hex().to_string()
    );
    assert_eq!(
        frame["state"]["body_hash"],
        frame["state"]["rendered_body_hash"]
    );
    assert_eq!(frame["state"]["caret"]["byte_offset"], 2);
    assert_eq!(frame["state"]["selected_page"], page.to_string());
    assert_eq!(frame["state"]["editor_binding_page"], page.to_string());
    assert_eq!(effect["before"]["body_chars"], 0);
    assert_ne!(effect["before"]["body_hash"], frame["state"]["body_hash"]);
    // This test uses an IME event, not a fabricated physical key serial:
    // display-latency qualification must still reject its absent physical start.
    assert!(effect["wayland_serial"].is_null());
    assert!(
        trace
            .drawn(Some(72), app.harness().native_input_snapshot())
            .is_none()
    );
    for row in [effect, frame] {
        let log = row.to_string();
        assert!(!log.contains("é"));
        assert!(!log.contains("Order"));
        assert!(row.get("text").is_none());
        assert!(row.get("input").is_none());
    }
}

#[test]
fn native_typing_evidence_tracks_canonical_and_rendered_text_without_logging_content() {
    let (mut app, page) = with_focused_page();
    let before = app
        .harness()
        .native_input_snapshot()
        .expect("Notes native observer");
    assert_eq!(before["bound_page"], page.to_string());
    assert_eq!(before["selected_page"], page.to_string());
    assert_eq!(before["body_chars"], 0);
    assert_eq!(before["editor_focused"], true);
    let mut text = String::new();
    for ch in ['h', 'i', 'é'] {
        app.harness().key(&ch.to_string());
        let _ = app.harness().poll_frame();
        text.push(ch);
        assert_eq!(body(&app, page), text);
        let observed = app
            .harness()
            .native_input_snapshot()
            .expect("typed native state");
        let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
        assert_eq!(observed["body_hash"], hash);
        assert_eq!(observed["rendered_body_hash"], hash);
        assert_eq!(observed["body_chars"], text.chars().count());
        assert_eq!(observed["rendered_body_chars"], text.chars().count());
        assert_eq!(observed["body_bytes"], text.len());
        assert_eq!(observed["caret"]["byte_offset"], text.len());
        assert_eq!(observed["bound_page"], page.to_string());
        assert_eq!(observed["editor_focused"], true);
        assert!(observed.get("body").is_none());
        assert!(observed.get("text").is_none());
    }
}
