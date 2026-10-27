//! What an agent does with a document through the published door: a
//! picture placed after the heading it names, a document the room holds
//! imported into an empty one (never over someone's writing), the document
//! kept as content, lists named as a reader sees them, and find and replace.

use serde_json::{Value, json};
use ui::framework::testing::TestApp;

use crate::framework::WordProcessorApp;

/// A valid 2×3 RGB PNG.
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 2, 0,
    0, 0, 54, 136, 73, 214, 0, 0, 0, 17, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 159, 1,
    140, 17, 20, 0, 68, 208, 5, 251, 176, 88, 73, 172, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
    130,
];

fn app() -> TestApp<WordProcessorApp> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<WordProcessorApp>::new().unwrap()
}

/// A document whose body is `markdown`, written as an edit of the body.
fn written(markdown: &str) -> TestApp<WordProcessorApp> {
    let mut app = app();
    let replaced = tilo_document::action::replace_body_action(markdown);
    app.invoke(ui::tilo_resource::Invoke::edit(&crate::model::edit(&replaced)).unwrap())
        .unwrap();
    app.settle();
    app
}

/// The body's blocks as (kind, text), in order.
fn blocks(app: &TestApp<WordProcessorApp>) -> Vec<(String, String)> {
    app.read_document(|doc| {
        let containers = crate::model::open(doc).unwrap();
        tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body)
            .into_iter()
            .map(|block| (block.kind, block.text))
            .collect()
    })
}

fn kinds(app: &TestApp<WordProcessorApp>) -> Vec<String> {
    blocks(app).into_iter().map(|(kind, _)| kind).collect()
}

#[test]
fn a_picture_goes_after_the_heading_its_text_names() {
    let mut app = written("# Summary\nSee the results below.\n# Results\nNumbers by region");
    let hash = app.content().put(PNG).unwrap();
    app.call(
        "place-image",
        json!({ "content": hash, "after": "results:" }),
    )
    .unwrap();
    app.settle();
    let shown = blocks(&app);
    let heading = shown
        .iter()
        .position(|(kind, text)| kind == "heading" && text == "Results")
        .unwrap();
    let image = shown
        .iter()
        .position(|(kind, _)| kind == "image")
        .unwrap_or_else(|| panic!("an image is placed: {shown:?}"));
    let numbers = shown
        .iter()
        .position(|(_, text)| text == "Numbers by region")
        .unwrap();
    assert!(
        heading < image && image < numbers,
        "under the Results heading, not the mention in the summary: {shown:?}"
    );
}

#[test]
fn a_picture_after_text_the_document_lacks_is_refused_with_its_headings() {
    let mut app = written("# Summary\nText\n# Budget\nMore");
    let hash = app.content().put(PNG).unwrap();
    let refused = app
        .call(
            "place-image",
            json!({ "content": hash, "after": "Appendix B" }),
        )
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("`Summary`, `Budget`"),
        "the refusal lists the headings: {refused}"
    );
    assert!(!kinds(&app).contains(&"image".to_owned()));
}

#[test]
fn a_document_kept_as_content_imports_into_an_empty_one_only() {
    let mut source = written("# Offsite\nBook the venue\n- lunch\n- dinner");
    let output = source
        .call_on_output(
            "word-processor.document",
            "export-content",
            json!({ "format": "docx" }),
        )
        .unwrap()
        .unwrap();
    let address = output.resources.first().cloned().unwrap();
    let hash = address.strip_prefix("tilo.content:").unwrap().to_owned();
    let bytes = source.content().read(&hash).unwrap();
    assert!(bytes.starts_with(b"PK"), "a Word file is a zip package");

    let mut target = app();
    target.content().put(&bytes).unwrap();
    let answer = target
        .call(
            "import-docx-content",
            json!({ "content": address, "name": "offsite.docx" }),
        )
        .unwrap()
        .unwrap_or(Value::Null);
    assert_eq!(answer["imported"], "offsite.docx");
    target.settle();
    let texts: Vec<String> = blocks(&target).into_iter().map(|(_, text)| text).collect();
    assert!(
        texts.iter().any(|text| text == "Book the venue"),
        "{texts:?}"
    );

    let refused = target
        .call("import-docx-content", json!({ "content": address }))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("already has writing"), "{refused}");
}

/// An import reads the format from the content, not the name it is given:
/// a Word file named without an extension imports, and a PDF is refused
/// naming what opens it.
#[test]
fn an_import_takes_the_format_its_content_holds() {
    let mut source = written("# Visa\nSofia Moreira");
    let output = source
        .call_on_output("word-processor.document", "export-content", json!({}))
        .unwrap()
        .unwrap();
    let address = output.resources.first().cloned().unwrap();
    let hash = address.strip_prefix("tilo.content:").unwrap().to_owned();
    let docx = source.content().read(&hash).unwrap();

    let mut target = app();
    target.content().put(&docx).unwrap();
    target
        .call(
            "import-docx-content",
            json!({ "content": address, "name": "Sofia details" }),
        )
        .unwrap();
    target.settle();
    assert!(
        blocks(&target)
            .iter()
            .any(|(_, text)| text == "Sofia Moreira"),
        "{:?}",
        blocks(&target)
    );

    let mut other = app();
    let pdf = other.content().put(b"%PDF-1.7\n%\xe2\xe3").unwrap();
    let refused = other
        .call(
            "import-docx-content",
            json!({ "content": format!("tilo.content:{pdf}"), "name": "details.docx" }),
        )
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("this is a PDF; use pdf-viewer import-pdf-content"),
        "{refused}"
    );
}

#[test]
fn set_paragraph_style_takes_bullet_and_numbered_as_list_items() {
    let mut app = written("Eggs\nMilk");
    let first = app.read_document(|doc| {
        let containers = crate::model::open(doc).unwrap();
        tilo_doc::model::region_blocks_in_order(doc, &containers, tilo_doc::model::Region::Body)
            .first()
            .cloned()
            .unwrap()
    });
    app.call(
        "set-paragraph-style",
        json!({ "kind": "numbered", "at": { "item": first } }),
    )
    .unwrap();
    app.settle();
    assert_eq!(kinds(&app).first().map(String::as_str), Some("list_item"));
}

#[test]
fn replace_text_rewrites_a_phrase_in_the_document() {
    let mut app = written("# Plan\nMeet on Monday at the office");
    app.call(
        "replace-text",
        json!({ "find": "Monday", "replace": "Tuesday" }),
    )
    .unwrap();
    app.settle();
    let missing = app
        .call("replace-text", json!({ "find": "Friday", "replace": "x" }))
        .unwrap_err()
        .to_string();
    assert!(
        missing.contains("`Friday` is not in the document"),
        "{missing}"
    );
    assert!(
        blocks(&app)
            .iter()
            .any(|(_, text)| text == "Meet on Tuesday at the office"),
        "{:?}",
        blocks(&app)
    );
}

#[test]
fn replace_body_writes_the_whole_body_from_markdown_tables_included() {
    let mut app = written("# Old\nGone");
    app.call(
        "replace-body",
        json!({ "markdown": "# Timeline\n\n| Time | Event |\n| --- | --- |\n| 14:02 | Alert |\n\nAfter the table." }),
    )
    .unwrap();
    app.settle();
    let cells = kinds(&app).iter().filter(|kind| *kind == "cell").count();
    assert_eq!(cells, 4, "a table of two rows of two is written");
    let shown = blocks(&app);
    assert!(
        shown
            .iter()
            .any(|(kind, text)| kind == "heading" && text == "Timeline")
            && shown.iter().any(|(_, text)| text == "After the table.")
            && !shown.iter().any(|(_, text)| text == "Gone"),
        "{shown:?}"
    );
}
