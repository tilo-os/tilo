//! What a reader outside the app is answered for a rich text: its paragraphs in their order.
//!
//! A read of a whole document as JSON (`tilo_automerge::json::read_document`, what every read
//! of a workspace's documents outside their app is answered with) cannot link this crate. It
//! orders paragraphs by the rule this crate's model orders them by, kept below both
//! (`tilo_automerge::rich_text`). These tests hold the two together: what the model says the
//! paragraphs are, and in which order, the read says.

use serde_json::Value;
use tilo_doc::{BlockView, Document};

/// `document` as a reader outside its app reads it: stored, loaded, and read as JSON.
fn read(document: &mut Document) -> Value {
    let stored = automerge::Automerge::load(&document.save()).unwrap();
    tilo_automerge::json::read_document(&stored).unwrap()
}

/// What the read's list `paragraphs` says of each paragraph: its id, its kind and its words.
fn said(paragraphs: &Value) -> Vec<(String, String, String)> {
    let word = |paragraph: &Value, key: &str| {
        paragraph
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    assert!(
        paragraphs.is_array(),
        "paragraphs read as a list in their order, not as {paragraphs}"
    );
    paragraphs
        .as_array()
        .unwrap()
        .iter()
        .map(|paragraph| {
            (
                word(paragraph, "id"),
                word(paragraph, "kind"),
                word(paragraph, "text"),
            )
        })
        .collect()
}

/// What the model says of each of `views`: its id, its kind and its words.
fn modelled(views: &[BlockView]) -> Vec<(String, String, String)> {
    views
        .iter()
        .map(|view| (view.id.clone(), view.kind.clone(), view.text.clone()))
        .collect()
}

#[test]
fn a_read_says_a_documents_paragraphs_in_their_order_with_their_kinds_and_words() {
    let mut poem = Document::new();
    // Written from the last line up: each paragraph goes in before the ones already there, so
    // the order they were written in, which is the order of their ids, is the reverse of the
    // order they read in.
    let last = poem.insert_block(None, "list_item");
    poem.splice_text(&last, 0, 0, "and so are you");
    let line = poem.insert_block(None, "paragraph");
    poem.splice_text(&line, 0, 0, "Roses are red");
    let title = poem.insert_block(None, "heading");
    poem.splice_text(&title, 0, 0, "A poem");
    let order = poem.blocks_in_order();
    assert_eq!(
        order,
        [title.clone(), line.clone(), last.clone()],
        "as the model orders them"
    );
    let mut by_id = order.clone();
    by_id.sort();
    assert_ne!(
        by_id, order,
        "the paragraphs' ids must not sort as the paragraphs read, or this shows nothing"
    );

    let read = read(&mut poem);
    assert_eq!(
        said(&read["blocks"]),
        [
            (title, "heading".to_owned(), "A poem".to_owned()),
            (line, "paragraph".to_owned(), "Roses are red".to_owned()),
            (last, "list_item".to_owned(), "and so are you".to_owned()),
        ]
    );
    // A paragraph's place is the list's order and stands nowhere else: bytes have no JSON
    // value.
    for paragraph in read["blocks"].as_array().unwrap() {
        assert_eq!(paragraph.get("order"), None, "{paragraph}");
    }
}

#[test]
fn a_read_orders_every_list_of_paragraphs_as_the_model_does() {
    let mut letter = Document::new();
    let greeting = letter.insert_block(None, "paragraph");
    letter.splice_text(&greeting, 0, 0, "Dear Ada,");
    let closing = letter.insert_block(Some(&greeting), "paragraph");
    letter.splice_text(&closing, 0, 0, "Yours, Bo");
    // One put between the two, one after it and taken out again, and a date written last and
    // moved to the top.
    let thanks = letter.insert_block(Some(&greeting), "paragraph");
    letter.splice_text(&thanks, 0, 0, "thank you for the jam.");
    let dropped = letter.insert_block(Some(&thanks), "paragraph");
    letter.splice_text(&dropped, 0, 0, "(a line thought better of)");
    let date = letter.insert_block(Some(&closing), "heading");
    letter.splice_text(&date, 0, 0, "Tuesday");
    letter.move_block(&date, None);
    letter.remove_block(&dropped);
    // The page's header and footer, a footnote, and a comment with a reply: each a list of
    // paragraphs of its own, each written last line first.
    for words in ["its second line", "its first line"] {
        let id = letter.insert_header_block(None, "paragraph");
        letter.splice_header_text(&id, 0, 0, words);
        let id = letter.insert_footer_block(None, "paragraph");
        letter.splice_footer_text(&id, 0, 0, words);
    }
    let note = letter.insert_footnote(&thanks, 5);
    for words in ["its second line", "its first line"] {
        let id = letter.insert_note_block(&note, None, "paragraph").unwrap();
        letter.splice_note_text(&note, &id, 0, 0, words);
    }
    let comment = letter.add_comment(&greeting, 0, 4, "Bo", "Too formal?");
    letter
        .add_comment_reply(&comment, "Ada", "2026-11-18", "No, keep it.")
        .unwrap();

    let model = letter.materialize();
    assert_eq!(
        model
            .blocks
            .iter()
            .map(|view| view.text.as_str())
            .collect::<Vec<_>>(),
        // The footnote's mark stands in the line it was put into.
        [
            "Tuesday",
            "Dear Ada,",
            "thank\u{fffc} you for the jam.",
            "Yours, Bo"
        ]
    );
    assert!(model.header.len() == 2 && model.footer.len() == 2);
    assert!(model.notes.iter().any(|note| note.blocks.len() >= 2));
    assert!(
        model
            .comments
            .iter()
            .any(|thread| !thread.replies.is_empty())
    );

    let read = read(&mut letter);
    assert_eq!(said(&read["blocks"]), modelled(&model.blocks));
    assert_eq!(said(&read["header"]), modelled(&model.header));
    assert_eq!(said(&read["footer"]), modelled(&model.footer));
    for note in &model.notes {
        assert_eq!(
            said(&read["notes"][&note.id]["blocks"]),
            modelled(&note.blocks)
        );
    }
    for comment in &model.comments {
        assert_eq!(
            said(&read["comments"][&comment.id]["blocks"]),
            modelled(&comment.blocks)
        );
        for reply in &comment.replies {
            assert_eq!(
                said(&read["comments"][&reply.id]["blocks"]),
                modelled(&reply.blocks)
            );
        }
    }
}

#[test]
fn a_read_orders_what_a_paragraph_holds_and_a_rich_text_kept_under_a_field() {
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _};

    // A rich text as the field of a larger document, as the word processor keeps its body.
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "Minutes").unwrap();
    let body = doc.put_object(ROOT, "body", ObjType::Map).unwrap();
    tilo_doc::model::init(&mut doc, &body);
    let mut minutes = Document::from_automerge_under(doc, &body).unwrap();
    let opening = minutes.insert_block(None, "paragraph");
    minutes.splice_text(&opening, 0, 0, "Present: Ada, Bo");
    let table = minutes.insert_block(None, "table");
    {
        // The table's cells, as a paragraph holds paragraphs: by id, each with its place.
        // Written b, a, c; placed c, a, b.
        let writer = minutes.as_mut();
        let (_, held) = writer
            .doc
            .get(&writer.c.blocks, table.as_str())
            .unwrap()
            .expect("the table is kept");
        let cells = writer
            .doc
            .put_object(&held, "children", ObjType::Map)
            .unwrap();
        for (id, place, words) in [
            ("b", vec![192_u8], "last cell"),
            ("a", vec![128], "middle cell"),
            ("c", vec![64], "first cell"),
        ] {
            let cell = writer.doc.put_object(&cells, id, ObjType::Map).unwrap();
            writer.doc.put(&cell, "kind", "cell").unwrap();
            writer.doc.put(&cell, "order", place).unwrap();
            let text = writer.doc.put_object(&cell, "text", ObjType::Text).unwrap();
            writer.doc.splice_text(&text, 0, 0, words).unwrap();
        }
    }

    let model = minutes.materialize();
    let read = read(&mut minutes);
    assert_eq!(read["title"], "Minutes");
    assert_eq!(said(&read["body"]["blocks"]), modelled(&model.blocks));
    let cells = &read["body"]["blocks"][0]["children"];
    assert_eq!(said(cells), modelled(&model.blocks[0].children));
    assert_eq!(
        said(cells)
            .iter()
            .map(|(_, _, words)| words.as_str())
            .collect::<Vec<_>>(),
        ["first cell", "middle cell", "last cell"]
    );
}
