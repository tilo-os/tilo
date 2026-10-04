//! Letters typed into a note land in the order they were typed, whichever
//! way they arrive: keys with frames in between, or input-method commits.

use notes::actions::new_page_action;
use notes::doc::NotesDoc;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use ui::winit::event::{Ime, WindowEvent};

fn with_focused_page() -> (TestApp<NotesShell>, PageId) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&new_page_action(page, None, 0, "Order")).unwrap())
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
        NotesDoc::page_block_snapshots(doc, page)
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
