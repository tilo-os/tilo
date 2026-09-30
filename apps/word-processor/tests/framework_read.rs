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
