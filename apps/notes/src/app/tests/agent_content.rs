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

/// An agent of the room, as the host admits one.
fn agent(name: &str) -> ui::tilo_actions::ActionInvoker {
    ui::tilo_actions::ActionInvoker::actor(name).with_kind(ui::tilo_actions::ActorKind::Agent)
}

/// `who` calls the notebook's `action` with `arguments`: its answer.
fn call_by(
    app: &mut ui::framework::testing::TestApp<crate::NotesShell>,
    who: &str,
    action: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, ui::tilo_actions::DoorError> {
    let answer = app.call_by(agent(who), "notes.notebook", action, arguments);
    app.settle();
    answer.map(|output| output.value.unwrap_or_default())
}

/// The note `who` makes titled `title`: its id.
fn made_by(
    app: &mut ui::framework::testing::TestApp<crate::NotesShell>,
    who: &str,
    title: &str,
) -> PageId {
    let answer = call_by(
        app,
        who,
        "create-page",
        serde_json::json!({ "title": title, "markdown": "Kept." }),
    )
    .unwrap();
    answer["page"].as_str().unwrap().parse().unwrap()
}

/// The text of each of `page`'s blocks.
fn block_texts(
    app: &ui::framework::testing::TestApp<crate::NotesShell>,
    page: PageId,
) -> Vec<String> {
    app.read_document(|doc| {
        crate::read::page_block_snapshots(doc, page)
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    })
}

#[test]
fn an_append_naming_no_note_lands_on_the_one_note_its_agent_made() {
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    // Another agent's notes come first, as the footage log did.
    let log = made_by(&mut app, "pia", "Footage log");
    made_by(&mut app, "pia", "Brief");
    let review = made_by(&mut app, "rhea", "Review v1");
    let before = block_texts(&app, log);

    let answer = call_by(
        &mut app,
        "rhea",
        "append-to-note",
        serde_json::json!({ "markdown": "v2 approved" }),
    )
    .unwrap();
    assert_eq!(answer["page"], review.to_string());
    assert_eq!(
        block_texts(&app, review).last().map(String::as_str),
        Some("v2 approved")
    );
    assert_eq!(
        block_texts(&app, log),
        before,
        "the first note is untouched"
    );
}

#[test]
fn an_append_naming_no_note_is_refused_unless_its_agent_made_exactly_one() {
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    let log = made_by(&mut app, "pia", "Footage log");
    let none = call_by(
        &mut app,
        "rhea",
        "append-to-note",
        serde_json::json!({ "markdown": "v2 approved" }),
    )
    .unwrap_err()
    .to_string();
    assert!(
        none.contains("you made none") && none.contains("“Footage log”"),
        "{none}"
    );
    made_by(&mut app, "rhea", "Review v1");
    made_by(&mut app, "rhea", "Review v2");
    let several = call_by(
        &mut app,
        "rhea",
        "append-to-note",
        serde_json::json!({ "title": " ", "markdown": "v2 approved" }),
    )
    .unwrap_err()
    .to_string();
    assert!(
        several.contains("“Review v1”") && several.contains("“Review v2”"),
        "the refusal names the agent's notes: {several}"
    );
    assert!(!several.contains("Footage log"), "{several}");
    assert_eq!(block_texts(&app, log), ["Kept."], "nothing was appended");
}

#[test]
fn an_append_finds_its_note_with_case_and_spacing_aside_and_never_another() {
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    let log = made_by(&mut app, "pia", "Footage log");
    let review = made_by(&mut app, "pia", "Review v1");
    call_by(
        &mut app,
        "rhea",
        "append-to-note",
        serde_json::json!({ "title": "  review   V1 ", "markdown": "Signed off." }),
    )
    .unwrap();
    assert_eq!(
        block_texts(&app, review).last().map(String::as_str),
        Some("Signed off."),
        "a named note wins, though the agent made none"
    );
    let unknown = call_by(
        &mut app,
        "pia",
        "append-to-note",
        serde_json::json!({ "title": "Review v2", "markdown": "Signed off." }),
    )
    .unwrap_err()
    .to_string();
    assert!(
        unknown.contains("no note is titled “Review v2”")
            && unknown.contains("“Footage log”")
            && unknown.contains("“Review v1”"),
        "the refusal names the notes there are: {unknown}"
    );
    assert_eq!(
        block_texts(&app, log),
        ["Kept."],
        "no fallback to another note"
    );
}

#[test]
fn each_body_is_published_under_its_notes_title() {
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    let log = made_by(&mut app, "pia", "Footage log");
    let review = made_by(&mut app, "rhea", "Review v1");
    let label = |app: &mut ui::framework::testing::TestApp<crate::NotesShell>, page| {
        let body = crate::actions::body_resource_id(page);
        app.actions()
            .unwrap()
            .into_iter()
            .find(|descriptor| descriptor.address.resource.as_str() == body)
            .map(|descriptor| descriptor.resource_label)
    };
    assert_eq!(label(&mut app, log).as_deref(), Some("Body of Footage log"));
    assert_eq!(
        label(&mut app, review).as_deref(),
        Some("Body of Review v1")
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
    assert!(
        markdown.starts_with("# Trip plan\n\n"),
        "the note's title is its first line: {markdown}"
    );
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
