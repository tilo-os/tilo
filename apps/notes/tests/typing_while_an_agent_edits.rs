//! A person typing in a note while an agent changes the same note: every letter they type lands,
//! in order, whatever the agent's change does and whenever it arrives — between two keys, before
//! a frame drew the last one, or as the note's first line is made.

use crate::framework_support::{open, surface};
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, ActorKind};

const TYPED: &str = "Typed by the person";

fn agent() -> ActionInvoker {
    ActionInvoker::actor("agent:claude-code")
        .with_kind(ActorKind::Agent)
        .acting_for("person")
}

fn note_titled(app: &TestApp<NotesShell>, title: &str) -> PageId {
    crate::framework_support::shown(app)
        .pages()
        .iter()
        .find(|page| page.title == title)
        .unwrap_or_else(|| panic!("a note titled {title}"))
        .id
}

fn text(app: &TestApp<NotesShell>, page: PageId) -> String {
    crate::framework_support::body(app, page).join("\n")
}

/// Click into the note's body, where a person starts typing.
fn click_into_body(app: &mut TestApp<NotesShell>) {
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("the note's body is drawn");
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    let _ = app.harness().poll_frame();
}

/// A note an agent made through the notebook's door, with `markdown` as its body.
fn agents_note(title: &str, markdown: Option<&str>) -> (TestApp<NotesShell>, PageId) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let kind = <notes::Notebook as ui::tilo_resource::Resource>::KIND;
    let mut args = serde_json::json!({ "title": title });
    if let Some(markdown) = markdown {
        args["markdown"] = markdown.into();
    }
    app.call_by(agent(), kind, "create-page", args).unwrap();
    let page = note_titled(&app, title);
    open(&mut app, page);
    let _ = app.harness().poll_frame();
    (app, page)
}

/// A paragraph the agent adds to `page` on its replica, merged as its change arrives.
fn agent_appends(app: &mut TestApp<NotesShell>, page: PageId, paragraph: &str) {
    app.merge_peer_edit(|doc| {
        crate::component_support::edit_page_body(
            doc,
            page,
            &tilo_document::RichTextAction::AppendParagraph {
                text: paragraph.to_owned(),
            },
        )
        .unwrap();
    });
}

#[test]
fn letters_typed_into_an_agents_empty_note_all_land() {
    let (mut app, page) = agents_note("Hello from Claude Code", None);
    click_into_body(&mut app);
    for letter in TYPED.chars() {
        app.harness().key(&letter.to_string());
        let _ = app.harness().poll_frame();
    }
    assert_eq!(text(&app, page), TYPED);
    let shown = surface(&mut app, |notes| notes.editor_text_for_test());
    assert!(
        shown.contains(TYPED),
        "the note shows what was typed: {shown:?}"
    );
}

#[test]
fn letters_typed_while_the_agent_adds_paragraphs_all_land_in_order() {
    let (mut app, page) = agents_note("Plan for Monday", Some("Agenda"));
    click_into_body(&mut app);
    for (index, letter) in TYPED.chars().enumerate() {
        app.harness().key(&letter.to_string());
        // The agent's changes arrive between keys, some before the frame that draws the key.
        if index % 3 == 0 {
            agent_appends(&mut app, page, &format!("Agent line {index}"));
        }
        if index % 2 == 0 {
            let _ = app.harness().poll_frame();
        }
    }
    let _ = app.harness().poll_frame();
    let body = text(&app, page);
    assert!(
        body.lines().any(|line| line.contains(TYPED)),
        "every typed letter landed, together and in order: {body:?}"
    );
}

#[test]
fn letters_typed_while_the_agent_writes_into_the_same_line_all_land() {
    let (mut app, page) = agents_note("Shared line", Some("Hello"));
    click_into_body(&mut app);
    for (index, letter) in TYPED.chars().enumerate() {
        app.harness().key(&letter.to_string());
        if index % 4 == 1 {
            app.merge_peer_edit(|doc| {
                crate::component_support::edit_page_body(
                    doc,
                    page,
                    &tilo_document::RichTextAction::ContinueParagraph { text: "!".into() },
                )
                .unwrap();
            });
        }
        let _ = app.harness().poll_frame();
    }
    let body = text(&app, page);
    let kept: String = body.chars().filter(|c| *c != '!').collect();
    assert!(
        kept.contains(TYPED),
        "the person's letters are all there, in order, around the agent's: {body:?}"
    );
}

#[test]
fn letters_typed_as_the_agent_fills_the_empty_note_all_land() {
    let (mut app, page) = agents_note("Filled meanwhile", None);
    click_into_body(&mut app);
    let mut letters = TYPED.chars();
    for letter in letters.by_ref().take(3) {
        app.harness().key(&letter.to_string());
    }
    // The agent's first line arrives before the frame that draws the first keys.
    agent_appends(&mut app, page, "Written by the agent");
    for letter in letters {
        app.harness().key(&letter.to_string());
        let _ = app.harness().poll_frame();
    }
    let body = text(&app, page);
    assert!(
        body.lines().any(|line| line.contains(TYPED)),
        "every typed letter landed: {body:?}"
    );
    assert!(body.contains("Written by the agent"), "{body:?}");
}
