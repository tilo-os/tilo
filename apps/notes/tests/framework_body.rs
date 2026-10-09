//! A note's body hosted by the framework: typing edits it through the
//! notebook's own action, and the published body resource reaches the same
//! page.

use notes::actions::{new_page_action, page_body_id};
use notes::doc::NotesDoc;
use notes::{NotesShell, PageId};
use tilo_document::{RichText, RichTextAction};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn with_page() -> (TestApp<NotesShell>, PageId) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&new_page_action(page, None, "a".into(), "First")).unwrap())
        .unwrap();
    (app, page)
}

fn body(app: &TestApp<NotesShell>, page: PageId) -> Vec<String> {
    app.read_document(|doc| {
        NotesDoc::page_block_snapshots(doc, page)
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    })
}

#[test]
fn typing_in_a_note_edits_its_body_through_the_notebook() {
    let (mut app, page) = with_page();
    let targets = app.harness().targets();
    let rect = targets
        .get("block_editor")
        .copied()
        .unwrap_or_else(|| panic!("no body editor among {:?}", targets.keys()));
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().type_text("hello world");
    assert_eq!(body(&app, page).concat(), "hello world");
    assert!(app.undo(), "typing is undoable");
    assert_eq!(
        body(&app, page).concat(),
        "hello ",
        "undo takes back a word"
    );
}

#[test]
fn the_published_body_applies_to_its_page() {
    let (mut app, page) = with_page();
    app.invoke_child::<RichText>(
        &page_body_id(page),
        Invoke::apply(&RichTextAction::AppendParagraph {
            text: "from an agent".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(
        body(&app, page).iter().any(|text| text == "from an agent"),
        "{:?}",
        body(&app, page)
    );
}

#[test]
fn the_published_body_offers_the_rich_text_views() {
    use ui::tilo_resource::Resource as _;
    let (app, page) = with_page();
    let children = notes::Notebook::children(&app.state());
    let note = children
        .iter()
        .find(|child| child.id.as_str() == notes::actions::page_resource_id(page))
        .expect("the note is published");
    let body = note
        .children
        .iter()
        .find(|child| child.id.as_str() == page_body_id(page))
        .expect("the note publishes its body");
    let views: Vec<&str> = body.views.iter().map(|view| view.id.as_str()).collect();
    assert_eq!(views, ["editor", "compact"], "{views:?}");
}

#[test]
fn importing_a_long_note_keeps_actions_usable_before_the_next_frame() {
    use std::fmt::Write as _;

    use notes::actions::{import_markdown_action, page_resource_id};
    use ui::bound_dispatch::BoundActionAddress;

    let mut app = crate::framework_support::notes();
    let page = PageId::new_v4();
    let paragraph = "collaborative notes ".repeat(14);
    let mut markdown = String::new();
    for index in 0..180 {
        writeln!(markdown, "Paragraph {index}: {paragraph}\n").unwrap();
    }
    let invoke = Invoke::<notes::Notebook>::apply(&import_markdown_action(
        page,
        "Long note",
        &markdown,
        Vec::new(),
    ))
    .unwrap();
    let address = BoundActionAddress {
        instance_id: app.harness().component_instance_id().unwrap(),
        address: tilo_actions::ActionAddress::new("notes.notebook", invoke.id),
    };
    // The control socket may request publication between applying an action
    // and drawing the next frame. Deliberately do not relayout here.
    app.harness()
        .invoke_bound_action_confirmed(&address, &invoke.arguments)
        .unwrap();
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "{violations:?}");
    assert_eq!(body(&app, page).len(), 180);

    app.call_on(&page_resource_id(page), "open", serde_json::json!({}))
        .unwrap();
    app.harness().relayout_at((1000, 300)).unwrap();
    crate::framework_support::send(
        &mut app,
        &winit::event::WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, -3.0),
            phase: winit::event::TouchPhase::Moved,
        },
    );
    app.harness().relayout_at((1000, 300)).unwrap();
    let scrolled = crate::framework_support::surface(&mut app, |notes| {
        notes.main_panel_mut().scroll_y_for_test()
    });
    assert!(
        scrolled > 0.0,
        "the published open action exposes a scrollable note"
    );
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().type_text("qualified input");
    assert!(body(&app, page).concat().contains("qualified input"));
    assert!(app.harness().component_contract_violations().is_empty());
}
