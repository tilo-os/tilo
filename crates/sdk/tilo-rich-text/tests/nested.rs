//! Rich text held by a record's field marked `#[model(resource)]` is a
//! resource of its own: published beneath the resource of the record that
//! holds it (an item's, or the record's own), typed as rich text, carrying
//! rich text's actions, each applied as the record's field edit of that one
//! field. Nothing of it is repeated on the record.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_document::RichText;
use ui::prelude::*;
use ui::serde_json::{self, json};

/// One letter.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Letter {
    /// Subject
    #[model(label)]
    pub subject: String,
    /// Body
    #[model(whole)]
    #[model(resource)]
    pub body: RichText,
}

/// Letters, and notes on them all.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Letters {
    pub letters: Collection<Letter>,
    /// Notes
    #[model(whole)]
    #[model(resource)]
    pub notes: RichText,
}

#[resource(kind = "test.letters", label = "Letters")]
impl Letters {
    /// Write a letter.
    #[action]
    pub fn write(&mut self, cx: &mut ActionCx, subject: String) {
        self.letters.push(
            cx.id(),
            Letter {
                subject,
                ..Letter::default()
            },
        );
    }
}

struct LettersApp;

impl ResourceApp for LettersApp {
    type Resource = Letters;
    const ID: &'static str = "so.tilo.test-letters";
    const TITLE: &'static str = "Letters";

    fn view(_cx: &mut ViewCx<Letters>) -> Element {
        Box::new(Column::new())
    }
}

fn open() -> TestApp<LettersApp> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<LettersApp>::new().unwrap();
    app.call("write", json!({ "subject": "Hello" })).unwrap();
    app.call("write", json!({ "subject": "Other" })).unwrap();
    app
}

/// The id of the letter titled `subject`.
fn letter(app: &TestApp<LettersApp>, subject: &str) -> Id {
    app.state()
        .letters
        .iter()
        .find(|(_, letter)| letter.subject == subject)
        .map(|(id, _)| id.clone())
        .expect("the letter is there")
}

fn body_text(app: &TestApp<LettersApp>, subject: &str) -> String {
    let id = letter(app, subject);
    app.state().letters.get(&id).unwrap().body.text()
}

#[test]
fn an_items_rich_text_is_published_beneath_the_item_as_rich_text() {
    let mut app = open();
    let hello = letter(&app, "Hello");
    let body = format!("test.letters/letters/{hello}/body");
    let surface = app.agent_surface();
    let rows: Vec<&serde_json::Value> = surface
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["resource"] == body.as_str())
        .collect();
    assert!(
        rows.iter()
            .all(|row| row["resource_kind"] == RichText::KIND),
        "{rows:?}"
    );
    let actions: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["action"].as_str())
        .collect();
    for declared in ["append-paragraph", "replace-body", "set-block-kind"] {
        assert!(actions.contains(&declared), "{declared}: {actions:?}");
    }
    // The record does not repeat them with a letter to name.
    assert!(
        !surface
            .as_array()
            .unwrap()
            .iter()
            .any(|row| { row["resource"] == Letters::KIND && row["action"] == "append-paragraph" }),
        "{surface}"
    );
}

#[test]
fn an_action_on_an_items_body_edits_that_body_alone_as_one_step() {
    let mut app = open();
    let hello = letter(&app, "Hello");
    let body = format!("test.letters/letters/{hello}/body");
    app.call_on(&body, "append-paragraph", json!({ "text": "Dear you" }))
        .unwrap();
    assert!(body_text(&app, "Hello").contains("Dear you"));
    assert!(!body_text(&app, "Other").contains("Dear you"));
    assert_eq!(app.state().notes.text().trim(), "");
    assert!(app.undo());
    assert!(!body_text(&app, "Hello").contains("Dear you"));
}

#[test]
fn a_records_own_rich_text_is_published_beneath_it() {
    let mut app = open();
    let notes = format!("{}/notes", Letters::KIND);
    app.call_on(
        &notes,
        "append-paragraph",
        json!({ "text": "post on Monday" }),
    )
    .unwrap();
    assert!(app.state().notes.text().contains("post on Monday"));
    assert!(!body_text(&app, "Hello").contains("post on Monday"));
}

#[test]
fn an_action_of_another_resource_is_refused_on_the_body() {
    let mut app = open();
    let hello = letter(&app, "Hello");
    let body = format!("test.letters/letters/{hello}/body");
    assert!(
        app.call_on(&body, "write", json!({ "subject": "x" }))
            .is_err()
    );
    assert_eq!(app.state().letters.len(), 2);
}
