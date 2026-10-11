//! The document operations an agent edits a document with by name, without
//! a caret: a paragraph aligned by its text, text replaced in place, and
//! Markdown appended at the end. Each keeps what it does not name — an image,
//! another writer's section — and what it sets survives a DOCX save.

use std::io::Read as _;
use std::path::Path;

use serde_json::json;
use tilo_actions::Action as _;
use tilo_doc::AttrVal;
use word_processor::actions::{ImageInput, RichTextAction, insert_image_action};

use crate::support::{App, apply, open, save_as, snapshot, texts};

/// The part `name` of the DOCX package at `path`.
fn docx_part(path: &Path, name: &str) -> String {
    let file = std::fs::File::open(path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut part = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut part)
        .unwrap();
    part
}

/// The names of the parts in the DOCX package at `path`.
fn docx_parts(path: &Path) -> Vec<String> {
    let file = std::fs::File::open(path).unwrap();
    let archive = zip::ZipArchive::new(file).unwrap();
    archive.file_names().map(str::to_owned).collect()
}

/// The `<w:p>` of `document` holding `text`.
fn paragraph_xml<'a>(document: &'a str, text: &str) -> &'a str {
    document
        .split("<w:p>")
        .chain(document.split("<w:p "))
        .find(|paragraph| paragraph.contains(text))
        .and_then(|paragraph| paragraph.split("</w:p>").next())
        .unwrap_or_else(|| panic!("no paragraph holds {text}: {document}"))
}

fn write_markdown(app: &mut App, markdown: &str) {
    app.call("write-markdown", json!({ "markdown": markdown }))
        .unwrap();
}

/// A small picture, stored where the app reads its content.
fn store_picture(app: &App) -> (String, Vec<u8>) {
    let png = ui::png::encode_rgba8(2, 2, &[200; 16]).unwrap();
    let hash = app.content().put(&png).unwrap();
    (hash, png)
}

#[test]
fn align_paragraph_centres_a_body_paragraph_and_docx_keeps_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agreement.docx");
    let mut app = open();
    write_markdown(
        &mut app,
        "# Agreement\n\nThe parties agree.\n\nSigned on behalf of both parties\n",
    );

    app.call(
        "align-paragraph",
        json!({ "text": "Signed on behalf", "align": "center" }),
    )
    .unwrap();
    app.call(
        "align-paragraph",
        json!({ "text": "The parties agree.", "align": "right" }),
    )
    .unwrap();

    let snap = snapshot(&app);
    let align = |text: &str| {
        snap.blocks
            .iter()
            .find(|block| block.text == text)
            .and_then(|block| block.attrs.get("align").cloned())
    };
    assert_eq!(
        align("Signed on behalf of both parties"),
        Some(AttrVal::Str("center".into()))
    );
    assert_eq!(
        align("The parties agree."),
        Some(AttrVal::Str("right".into()))
    );
    assert_eq!(align("Agreement"), None, "only the named paragraphs");

    save_as(&mut app, &path);
    let document = docx_part(&path, "word/document.xml");
    assert!(
        paragraph_xml(&document, "Signed on behalf of both parties")
            .contains(r#"<w:jc w:val="center"/>"#),
        "{document}"
    );
    assert!(
        paragraph_xml(&document, "The parties agree.").contains(r#"<w:jc w:val="right"/>"#),
        "{document}"
    );

    let missing = app
        .call(
            "align-paragraph",
            json!({ "text": "Witnessed by", "align": "center" }),
        )
        .unwrap_err()
        .to_string();
    assert!(missing.contains("Witnessed by"), "{missing}");
    assert!(
        app.call(
            "align-paragraph",
            json!({ "text": "Agreement", "align": "middle" }),
        )
        .is_err(),
        "an alignment Writer has not"
    );
}

#[test]
fn replace_text_edits_in_place_and_keeps_an_inserted_image() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("poster.docx");
    let mut app = open();
    write_markdown(
        &mut app,
        "# Summer fair\n\n**Venue:** St. Mary's Hall, Saturday.\n\nSee you at St. Mary's Hall!\n",
    );
    let (hash, png) = store_picture(&app);
    let venue = snapshot(&app)
        .blocks
        .into_iter()
        .find(|block| block.text.starts_with("Venue"))
        .unwrap();
    apply(
        &mut app,
        &insert_image_action(
            &venue.id,
            venue.text.chars().count(),
            ImageInput::of(hash.clone(), &png),
        ),
    );

    app.call(
        "replace-text",
        json!({ "find": "St. Mary's Hall", "replace": "the Town Hall" }),
    )
    .unwrap();
    assert!(
        texts(&app).contains(&"See you at St. Mary's Hall!".to_owned()),
        "the first occurrence only: {:?}",
        texts(&app)
    );
    app.call(
        "replace-text",
        json!({ "find": "St. Mary's Hall", "replace": "the Town Hall", "all": true }),
    )
    .unwrap();

    let snap = snapshot(&app);
    let paragraphs: Vec<&str> = snap
        .blocks
        .iter()
        .filter(|block| block.kind != "image")
        .map(|block| block.text.as_str())
        .filter(|text| !text.is_empty())
        .collect();
    assert_eq!(
        paragraphs,
        [
            "Summer fair",
            "Venue: the Town Hall, Saturday.",
            "See you at the Town Hall!"
        ]
    );
    let image = snap
        .blocks
        .iter()
        .find(|block| block.kind == "image")
        .expect("the image stays");
    assert_eq!(
        image.attrs.get("blob"),
        Some(&AttrVal::Str(hash)),
        "{image:?}"
    );
    let heading = snap.blocks.iter().find(|block| block.kind == "heading");
    assert_eq!(
        heading.map(|block| block.text.as_str()),
        Some("Summer fair")
    );
    let venue = snap
        .blocks
        .iter()
        .find(|block| block.text.starts_with("Venue"))
        .unwrap();
    assert!(
        venue.marks.iter().any(|mark| mark.name == "strong"),
        "the paragraph keeps its formatting: {venue:?}"
    );

    save_as(&mut app, &path);
    assert!(
        docx_parts(&path)
            .iter()
            .any(|part| part.starts_with("word/media/")),
        "the DOCX embeds the image: {:?}",
        docx_parts(&path)
    );

    let missing = app
        .call(
            "replace-text",
            json!({ "find": "St. Mary's Hall", "replace": "x" }),
        )
        .unwrap_err()
        .to_string();
    assert!(missing.contains("not in the document"), "{missing}");
}

#[test]
fn append_markdown_adds_at_the_end_and_keeps_what_is_there() {
    let mut app = open();
    app.call("append-markdown", json!({ "markdown": "# Onboarding" }))
        .unwrap();
    assert_eq!(texts(&app), ["Onboarding"], "no blank line above the text");
    app.call(
        "append-markdown",
        json!({ "markdown": "Welcome aboard.\n\n## IT setup\n\n- Laptop\n- Badge\n" }),
    )
    .unwrap();
    assert_eq!(
        texts(&app),
        [
            "Onboarding",
            "Welcome aboard.",
            "IT setup",
            "Laptop",
            "Badge"
        ]
    );
    let kinds: Vec<String> = snapshot(&app)
        .blocks
        .into_iter()
        .map(|block| block.kind)
        .collect();
    assert_eq!(
        kinds,
        ["heading", "paragraph", "heading", "list_item", "list_item"]
    );

    // Another writer appends its section from the same version at the same
    // time: both sections arrive whole, and nothing that was there goes.
    let mut peer = app.peer_document();
    RichTextAction::AppendMarkdown {
        markdown: "## Benefits\n\nHealth plan.".into(),
    }
    .apply(&mut peer)
    .unwrap();
    peer.commit();
    app.call(
        "append-markdown",
        json!({ "markdown": "## First week\n\nMeet the team." }),
    )
    .unwrap();
    app.merge_peer(&mut peer).unwrap();
    let merged = texts(&app);
    assert_eq!(merged.len(), 9, "{merged:?}");
    assert_eq!(
        merged[..5],
        [
            "Onboarding",
            "Welcome aboard.",
            "IT setup",
            "Laptop",
            "Badge"
        ]
    );
    let tail = merged[5..].join("|");
    assert!(
        tail.contains("Benefits|Health plan.") && tail.contains("First week|Meet the team."),
        "{merged:?}"
    );

    assert!(
        app.call("append-markdown", json!({ "markdown": "  \n" }))
            .is_err(),
        "nothing to append"
    );
}
