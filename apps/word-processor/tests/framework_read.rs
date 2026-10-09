//! An agent reads the document through the bound-action door the broker
//! and the MCP bridge reach: `read-text` pages the committed body text.

use ui::bound_dispatch::BoundActionAddress;
use ui::framework::testing::TestApp;
use word_processor::framework::WordProcessorApp;

const INSTANCE: u64 = 7;

fn read_text_address(resources: &[tilo_actions::Resource]) -> Option<tilo_actions::ActionAddress> {
    resources.iter().find_map(|resource| {
        resource
            .actions
            .iter()
            .find(|action| action.id == "read-text")
            .map(|action| tilo_actions::ActionAddress::new(resource.id.as_str(), &action.id))
            .or_else(|| read_text_address(&resource.children))
    })
}

fn read(
    app: &mut TestApp<WordProcessorApp>,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, tilo_actions::DoorError> {
    let harness = app.harness();
    let address = BoundActionAddress {
        instance_id: INSTANCE,
        address: read_text_address(&harness.resources()).expect("read-text is published"),
    };
    harness
        .invoke_bound_action(&address, arguments.as_object().unwrap())
        .map(|started| {
            started
                .completed()
                .and_then(|output| output.value)
                .unwrap_or_default()
        })
}

#[test]
fn read_text_pages_the_committed_document_for_an_agent() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("Q3 Planning");
    app.harness().key("Enter");
    app.harness().type_text("Book the venue");

    let whole = read(&mut app, &serde_json::json!({})).unwrap();
    let markdown = whole["content"]["markdown"].as_str().unwrap();
    assert!(
        markdown.starts_with("Q3 Planning {#") && markdown.contains("\nBook the venue {#"),
        "{markdown}"
    );

    let first = read(
        &mut app,
        &serde_json::json!({"pageSize": 1, "format": "segments"}),
    )
    .unwrap();
    assert_eq!(first["version"], 1);
    assert_eq!(first["content"]["segments"][0]["text"], "Q3 Planning");
    assert!(!first["basis"]["heads"].as_array().unwrap().is_empty());
    let cursor = first["nextCursor"]
        .as_str()
        .expect("a second page")
        .to_owned();

    let rest = read(
        &mut app,
        &serde_json::json!({"pageSize": 8, "cursor": cursor, "format": "segments"}),
    )
    .unwrap();
    assert_eq!(rest["content"]["segments"][0]["text"], "Book the venue");
    assert!(rest["nextCursor"].is_null());
    assert_eq!(
        rest["record"], first["record"],
        "one record, stamped by the SDK"
    );

    app.harness().type_text("!");
    let stale = read(
        &mut app,
        &serde_json::json!({"pageSize": 8, "cursor": cursor, "format": "segments"}),
    );
    assert!(
        matches!(&stale, Err(tilo_actions::DoorError::Unavailable { code, .. }) if code == "stale_cursor"),
        "a cursor from before an edit is refused: {stale:?}"
    );
}

fn invoke(
    app: &mut TestApp<WordProcessorApp>,
    action: &str,
    arguments: &serde_json::Value,
) -> Option<serde_json::Value> {
    fn address(
        resources: &[tilo_actions::Resource],
        id: &str,
    ) -> Option<tilo_actions::ActionAddress> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|action| action.id == id)
                .map(|action| tilo_actions::ActionAddress::new(resource.id.as_str(), &action.id))
                .or_else(|| address(&resource.children, id))
        })
    }
    let harness = app.harness();
    let address = BoundActionAddress {
        instance_id: INSTANCE,
        address: address(&harness.resources(), action).expect("the action is published"),
    };
    let (invocation, started) = harness
        .start_bound_job(&address, arguments.as_object().unwrap(), false)
        .unwrap();
    // An edit at the caret is applied as the frame settles; one that answers
    // with what it changed does so then.
    harness.relayout();
    if let Some(output) = started.completed() {
        return output.value;
    }
    for _ in 0..4 {
        if let Some(update) = harness
            .job_updates(invocation)
            .iter()
            .find(|update| update.is_terminal())
        {
            return match update {
                tilo_actions::InvocationUpdate::Succeeded { output, .. } => output.value.clone(),
                other => panic!("{action} failed: {other:?}"),
            };
        }
        harness.relayout();
    }
    panic!("{action} never answered")
}

#[test]
fn an_edit_answers_with_the_blocks_it_changed() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("Draft");
    invoke(&mut app, "select-all", &serde_json::json!({}));
    let pasted = invoke(
        &mut app,
        "paste",
        &serde_json::json!({"markdown": "# Standup\n\nMaya is out\n"}),
    );
    let summary = pasted
        .as_ref()
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert!(
        summary.starts_with("2 changed:\n# Standup {#") && summary.contains("\nMaya is out {#"),
        "{pasted:?}"
    );
    let styled = invoke(
        &mut app,
        "set-paragraph-style",
        &serde_json::json!({"kind": "heading", "level": 2}),
    );
    assert!(
        styled
            .as_ref()
            .and_then(|value| value.as_str())
            .is_some_and(|summary| summary.starts_with("1 changed:\n## ")
                && summary.matches(" {#").count() == 1),
        "{styled:?}"
    );
}

/// `text` with every `{#id}` and `{#table id}` emptied, so a test can state
/// the layout it expects without the generated ids.
fn without_ids(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some((before, tag)) = rest.split_once("{#") {
        out.push_str(before);
        let (id, after) = tag.split_once('}').expect("a closed tag");
        out.push_str(if id.starts_with("table ") {
            "{#table}"
        } else {
            "{#}"
        });
        rest = after;
    }
    out + rest
}

/// The id closing the line of `markdown` holding `text`, cell or block.
fn id_of(markdown: &str, text: &str) -> String {
    let (_, tagged) = markdown
        .split_once(&format!("{text} {{#"))
        .expect("the text is read");
    tagged.split_once('}').unwrap().0.to_owned()
}

#[test]
fn a_heading_list_and_table_read_back_with_the_handles_that_edit_them() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("x");
    invoke(&mut app, "select-all", &serde_json::json!({}));
    invoke(
        &mut app,
        "paste",
        &serde_json::json!({"markdown": "## Standup\n\n- review\n  1. screen reader\n- ship\n\n\
            | Person | Blocked | Next |\n| --- | --- | --- |\n| Maya | no | docs |\n| Ravi | yes | CI |\n"}),
    );
    let layout = "## Standup {#}\n\
        - review {#}\n  1. screen reader {#}\n- ship {#}\n\
        | Person {#} | Blocked {#} | Next {#} | {#table}\n| --- | --- | --- |\n\
        | Maya {#} | no {#} | docs {#} |\n| Ravi {#} | yes {#} | CI {#} |\n\
        {#}\n";

    let read_back = read(&mut app, &serde_json::json!({})).unwrap();
    let markdown = read_back["content"]["markdown"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(without_ids(&markdown), layout, "{markdown}");

    let cell = id_of(&markdown, "yes");
    invoke(
        &mut app,
        "select-block",
        &serde_json::json!({"block": cell}),
    );
    invoke(&mut app, "type-text", &serde_json::json!({"text": "no"}));
    let markdown = read(&mut app, &serde_json::json!({})).unwrap()["content"]["markdown"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        markdown.contains(&format!(
            "| Ravi {{#{}}} | no {{#{cell}}} |",
            id_of(&markdown, "Ravi")
        )),
        "the cell's id selects it: {markdown}"
    );

    // The table read names is the one a row is added to, and the next read
    // shows the row.
    let table = markdown
        .split_once("{#table ")
        .and_then(|(_, tagged)| tagged.split_once('}'))
        .map(|(id, _)| id.to_owned())
        .expect("the table is tagged with its id");
    invoke(&mut app, "add-row", &serde_json::json!({ "table": table }));
    let markdown = read(&mut app, &serde_json::json!({})).unwrap()["content"]["markdown"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        without_ids(&markdown).contains("| Ravi {#} | no {#} | CI {#} |\n| {#} | {#} | {#} |"),
        "{markdown}"
    );
}

/// A paste and a row added answer with the blocks they made, so the caller
/// fills them without reading the document again.
#[test]
#[ignore = "an edit effect answers before its edit applies, a verb has no answer: GAPS 'edit effects answer with what they changed', 'answer'"]
fn pasting_a_table_and_adding_a_row_answer_with_the_blocks_they_made() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("x");
    invoke(&mut app, "select-all", &serde_json::json!({}));
    let pasted = invoke(
        &mut app,
        "paste",
        &serde_json::json!({"markdown": "## Standup\n\n- review\n  1. screen reader\n- ship\n\n\
            | Person | Blocked | Next |\n| --- | --- | --- |\n| Maya | no | docs |\n| Ravi | yes | CI |\n"}),
    );
    let layout = "## Standup {#}\n\
        - review {#}\n  1. screen reader {#}\n- ship {#}\n\
        | Person {#} | Blocked {#} | Next {#} | {#table}\n| --- | --- | --- |\n\
        | Maya {#} | no {#} | docs {#} |\n| Ravi {#} | yes {#} | CI {#} |\n\
        {#}\n";
    let pasted = pasted
        .as_ref()
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert_eq!(
        without_ids(pasted),
        format!("14 changed:\n{layout}"),
        "{pasted}"
    );
    app.harness().relayout();
    let appended = invoke(&mut app, "add-row", &serde_json::json!({}));
    let appended = appended
        .as_ref()
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert_eq!(
        without_ids(appended),
        "3 changed:\n| {#} | {#} | {#} | {#table}\n| --- | --- | --- |\n",
        "the new row answers with the ids that fill it"
    );
}
