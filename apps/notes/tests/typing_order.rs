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
    // Discarded buffers retain the same witness on bounded subsequent draws.
    for id in 72..103 {
        let retry = trace
            .drawn(Some(id), app.harness().native_input_snapshot())
            .expect("bounded retry witness");
        assert_eq!(retry["cpu_frame_id"], id);
        assert_eq!(retry["input_ids"], frame["input_ids"]);
        assert_eq!(retry["state"], frame["state"]);
        assert_eq!(retry["observer_session"], frame["observer_session"]);
        assert!(!retry.to_string().contains("é"));
    }
    assert!(
        trace
            .drawn(Some(103), app.harness().native_input_snapshot())
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

#[test]
fn semantic_new_page_open_adopts_focused_line_and_types_without_pointer_focus() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call(
        "new-note",
        serde_json::json!({"title": "Input diagnostic scratch"}),
    )
    .unwrap();
    app.settle();
    let page = app.read_document(read::list_pages)[0].id;
    app.call_on(
        &notes::actions::page_resource_id(page),
        "open",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    let before = app
        .harness()
        .native_input_snapshot()
        .expect("Notes observer");
    assert_eq!(before["editor_binding_page"], page.to_string());
    assert_eq!(before["body_chars"], 0);
    assert_eq!(before["editor_focused"], true);
    assert_eq!(before["active_line_present"], true);
    assert_eq!(before["active_line_focused"], true);
    assert_eq!(before["active_line_disabled"], false);
    assert_eq!(before["editor_composing"], false);
    assert_eq!(before["sidebar_renaming"], false);
    app.harness().key("a");
    assert_eq!(
        body(&app, page),
        "a",
        "semantic Open must admit ordinary typing without a pointer workaround"
    );
}

#[test]
fn semantic_open_from_a_narrow_drawer_routes_the_first_printable_key_to_the_note() {
    use crate::framework_support::surface;
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call("new-note", serde_json::json!({"title": "Routing scratch"}))
        .unwrap();
    app.settle();
    let page = app.read_document(read::list_pages)[0].id;
    app.harness().relayout_at((780, 720)).unwrap();
    assert!(surface(&mut app, |notes| notes.built_narrow_for_test()));
    let toggle = surface(&mut app, |notes| notes.menu_btn_center_for_test());
    app.harness().click_at(toggle.x, toggle.y);
    assert!(surface(&mut app, |notes| notes.drawer_open_for_test()));
    let blocked = app.harness().native_input_snapshot().unwrap();
    assert_eq!(blocked["sidebar_drawer_open"], true);
    assert_eq!(blocked["editor_route_available"], false);
    app.call("open-note", serde_json::json!({"page": page}))
        .unwrap();
    app.settle();
    let before = app.harness().native_input_snapshot().unwrap();
    assert_eq!(before["active_line_focused"], true);
    assert_eq!(before["active_line_disabled"], false);
    assert_eq!(before["body_chars"], 0);
    app.harness().key("a");
    assert_eq!(
        body(&app, page),
        "a",
        "semantic Open must release the drawer's modal keyboard capture"
    );
}

#[test]
fn semantic_new_page_from_a_narrow_drawer_routes_the_first_printable_key_to_the_note() {
    use crate::framework_support::surface;
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call("new-note", serde_json::json!({"title": "Routing scratch"}))
        .unwrap();
    app.settle();
    app.harness().relayout_at((780, 720)).unwrap();
    assert!(surface(&mut app, |notes| notes.built_narrow_for_test()));
    let toggle = surface(&mut app, |notes| notes.menu_btn_center_for_test());
    app.harness().click_at(toggle.x, toggle.y);
    assert!(surface(&mut app, |notes| notes.drawer_open_for_test()));
    app.call(
        "new-note",
        serde_json::json!({"title": "Fresh routing scratch"}),
    )
    .unwrap();
    app.settle();
    let selected = app.harness().native_input_snapshot().unwrap()["selected_page"]
        .as_str()
        .unwrap()
        .to_owned();
    let page = PageId::parse_str(&selected).unwrap();
    app.call("open-note", serde_json::json!({"page": page}))
        .unwrap();
    app.settle();
    let before = app.harness().native_input_snapshot().unwrap();
    assert_eq!(before["active_line_focused"], true);
    assert_eq!(before["active_line_disabled"], false);
    assert_eq!(before["body_chars"], 0);
    app.harness().key("a");
    assert_eq!(
        body(&app, page),
        "a",
        "semantic Open must release the drawer's modal keyboard capture"
    );
}

#[test]
fn focused_editor_behind_a_retained_narrow_drawer_does_not_lose_printable_input_after_open() {
    use crate::framework_support::surface;
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call("new-note", serde_json::json!({"title": "Routing scratch"}))
        .unwrap();
    app.settle();
    app.harness().relayout_at((780, 720)).unwrap();
    assert!(surface(&mut app, |notes| notes.built_narrow_for_test()));
    let toggle = surface(&mut app, |notes| notes.menu_btn_center_for_test());
    app.harness().click_at(toggle.x, toggle.y);
    assert!(surface(&mut app, |notes| notes.drawer_open_for_test()));
    app.call(
        "new-note",
        serde_json::json!({"title": "Fresh routing scratch"}),
    )
    .unwrap();
    app.settle();
    let selected = app.harness().native_input_snapshot().unwrap()["selected_page"]
        .as_str()
        .unwrap()
        .to_owned();
    let page = PageId::parse_str(&selected).unwrap();
    app.call("open-note", serde_json::json!({"page": page}))
        .unwrap();
    app.settle();
    app.harness()
        .perform_named("block_editor", &ui::a11y::Action::Focus)
        .unwrap();
    let before = app.harness().native_input_snapshot().unwrap();
    assert_eq!(before["active_line_focused"], true);
    assert_eq!(before["active_line_disabled"], false);
    assert_eq!(before["body_chars"], 0);
    app.harness().key("a");
    assert_eq!(
        body(&app, page),
        "a",
        "semantic Open must release the drawer's modal keyboard capture"
    );
}

#[test]
fn semantic_open_preserves_a_docked_sidebar_and_the_existing_body_caret() {
    use crate::framework_support::surface;
    let (mut app, page) = with_focused_page();
    app.harness().relayout_at((1280, 720)).unwrap();
    assert!(!surface(&mut app, |notes| notes.built_narrow_for_test()));
    assert!(surface(&mut app, |notes| notes.drawer_open_for_test()));
    app.harness().type_text("hi");
    app.harness().key("ArrowLeft");
    let before = app.harness().native_input_snapshot().unwrap();
    app.call("open-note", serde_json::json!({"page": page}))
        .unwrap();
    app.settle();
    let after = app.harness().native_input_snapshot().unwrap();
    assert_eq!(after["caret"], before["caret"]);
    assert_eq!(after["sidebar_drawer_open"], false);
    assert_eq!(after["editor_route_available"], true);
    assert!(surface(&mut app, |notes| notes.drawer_open_for_test()));
    app.harness().key("x");
    assert_eq!(body(&app, page), "hxi");
}

#[test]
fn semantic_open_commits_an_outgoing_title_only_to_its_original_page() {
    use crate::framework_support::surface;
    let (mut app, original) = with_focused_page();
    let other = PageId::new_v4();
    app.invoke(Invoke::apply(&create_page_action(other, None, "Other")).unwrap())
        .unwrap();
    app.call("open-note", serde_json::json!({"page": original}))
        .unwrap();
    app.settle();
    let title = surface(&mut app, |notes| notes.main_title_center_for_test());
    app.harness().click_at(title.x, title.y);
    assert!(surface(&mut app, |notes| notes.main_title_editing_for_test()));
    app.harness().type_text("Renamed");
    app.call("open-note", serde_json::json!({"page": other}))
        .unwrap();
    app.settle();
    app.read_document(|doc| {
        let pages = read::list_pages(doc);
        assert_eq!(
            pages.iter().find(|page| page.id == original).unwrap().title,
            "Renamed"
        );
        assert_eq!(
            pages.iter().find(|page| page.id == other).unwrap().title,
            "Other"
        );
    });
    let editor = app.harness().native_input_snapshot().unwrap();
    assert_eq!(editor["editor_binding_page"], other.to_string());
    assert_eq!(editor["active_line_focused"], true);
    app.harness().key("a");
    assert_eq!(body(&app, other), "a");
    assert_eq!(body(&app, original), "");
}
