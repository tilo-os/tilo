//! What an agent makes in Notes without a file chooser: Markdown appended
//! to a note it names by title, and exports kept as the room's content.

use super::*;

/// Make a note titled `title` with `markdown` as its body.
fn note(app: &mut ui::framework::testing::TestApp<crate::NotesShell>, title: &str, markdown: &str) {
    app.call(
        "create-page",
        serde_json::json!({ "title": title, "markdown": markdown }),
    )
    .unwrap();
    app.settle();
}

/// The text of the content an export answered with.
fn exported(
    app: &ui::framework::testing::TestApp<crate::NotesShell>,
    answer: &serde_json::Value,
) -> String {
    let address = answer["content"].as_str().unwrap();
    let hash = ui::tilo_actions::content_hash(address).unwrap();
    String::from_utf8(app.content().read(hash).unwrap()).unwrap()
}

#[test]
fn append_to_note_adds_markdown_to_the_note_named_by_title_ignoring_case() {
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    note(&mut app, "Meeting notes", "First point.");
    let answer = app
        .call(
            "append-to-note",
            serde_json::json!({ "title": "meeting NOTES", "markdown": "- follow up with Sam" }),
        )
        .unwrap()
        .unwrap();
    app.settle();
    let page = page_ids(&app.state())[0];
    assert_eq!(answer["page"], page.to_string());
    let texts: Vec<String> = app.read_document(|doc| {
        crate::read::page_block_snapshots(doc, page)
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    });
    assert_eq!(
        texts.last().map(String::as_str),
        Some("follow up with Sam"),
        "the Markdown lands after what the note held: {texts:?}"
    );
    assert!(texts.iter().any(|text| text == "First point."));
}

#[test]
fn append_to_note_refuses_a_title_no_note_carries() {
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    note(&mut app, "Groceries", "Milk");
    let refused = app
        .call(
            "append-to-note",
            serde_json::json!({ "title": "Errands", "markdown": "Bread" }),
        )
        .unwrap_err();
    assert!(
        refused.to_string().contains("no note is titled"),
        "{refused}"
    );
}

#[test]
fn export_markdown_content_keeps_one_note_as_content_by_title() {
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    note(&mut app, "Trip plan", "# Day one\n\nVisit the harbour.");
    note(&mut app, "Other", "Unrelated.");
    let answer = app
        .call(
            "export-markdown-content",
            serde_json::json!({ "title": "trip plan" }),
        )
        .unwrap()
        .unwrap();
    assert_eq!(answer["notes"], 1);
    assert_eq!(answer["name"], "trip-plan.md");
    let markdown = exported(&app, &answer);
    assert!(markdown.contains("Visit the harbour."), "{markdown}");
    assert!(!markdown.contains("Unrelated."), "{markdown}");
}

#[test]
fn export_markdown_content_without_a_note_keeps_the_whole_notebook_under_title_headings() {
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    note(&mut app, "Alpha", "First body.");
    note(&mut app, "Beta", "Second body.");
    let answer = app
        .call("export-markdown-content", serde_json::json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(answer["notes"], 2);
    let markdown = exported(&app, &answer);
    let alpha = markdown.find("# Alpha").unwrap();
    let beta = markdown.find("# Beta").unwrap();
    assert!(alpha < beta, "notebook order: {markdown}");
    assert!(markdown.contains("Second body."));
}
