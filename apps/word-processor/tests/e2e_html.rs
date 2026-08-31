//! End-to-end: HTML *import* via the word processor's open path. HTML *export*
//! is provided by `qdoc-html`; this proves the read side — a `.html` file (whether
//! hand-written or produced by `qdoc_html::render_html`) opens into the canonical
//! CRDT model with its headings, paragraphs, bold runs, and links intact.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;

use ui::Widget;
use word_processor::{WordProcessorWidget, WpFormat, open_document};

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wp-html-e2e-{}-{}", std::process::id(), name));
    p
}

/// Author a document in the canonical model, then return its `qdoc-html`-rendered
/// HTML — the exact bytes the editor's HTML *export* would write.
fn render_doc_to_html() -> String {
    let mut state = ui::State::new();
    let c = qdoc::model::init(&mut state.doc, &state.root);
    let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
    {
        let mut dm = qdoc::DocMut {
            doc: &mut state.doc,
            c: &c,
            ids: &mut ids,
        };
        let h = dm.insert_block(None, "heading");
        dm.set_attr(&h, "level", 1_i64);
        dm.splice_text(&h, 0, 0, "Report");

        let p = dm.insert_block(Some(&h), "paragraph");
        // "See bold and a link here": mark "bold" strong, "a link" as a link.
        dm.splice_text(&p, 0, 0, "See bold and a link here");
        dm.add_mark(&p, 4, 8, "strong", true); // "bold"
        dm.add_mark(&p, 13, 19, "link", "https://qos.dev/"); // "a link"
    }
    let snap = qdoc::model::materialize(&state.doc, &c);
    qdoc_html::render_html(&snap, qdoc_html::HtmlOptions::default())
}

#[test]
fn open_html_recovers_headings_bold_link() {
    // export (qdoc-html) → file → import (HtmlCodec) round-trip.
    let html = render_doc_to_html();
    let path = temp_path("doc.html");
    std::fs::write(&path, html.as_bytes()).unwrap();

    let (mut state, mut wp, fmt) = open_document(&path).unwrap();
    assert_eq!(fmt, WpFormat::Html, "extension picks the HTML format");
    wp.sync(&mut state.doc, &state.root);

    let snap = qdoc::model::materialize(&state.doc, &wp_containers(&wp, &state));

    // The first block is the H1 heading "Report".
    let first = &snap.blocks[0];
    assert_eq!(first.kind, "heading", "first block is a heading: {snap:?}");
    assert_eq!(first.text, "Report");
    assert_eq!(first.attrs.get("level"), Some(&qdoc::AttrVal::Int(1)));

    // The paragraph survives with the bold run and the link mark.
    let p = snap
        .blocks
        .iter()
        .find(|b| b.kind == "paragraph")
        .expect("paragraph survives");
    assert_eq!(p.text, "See bold and a link here");
    let strong = p
        .marks
        .iter()
        .find(|m| m.name == "strong")
        .expect("bold run survives");
    assert_eq!(&p.text[strong.start..strong.end], "bold", "{:?}", p.marks);
    let link = p
        .marks
        .iter()
        .find(|m| m.name == "link")
        .expect("link survives");
    assert_eq!(
        link.value,
        qdoc::document::MarkValue::Str("https://qos.dev/".into())
    );
    assert_eq!(&p.text[link.start..link.end], "a link", "{:?}", p.marks);

    std::fs::remove_file(&path).ok();
}

#[test]
fn open_small_handwritten_html_first_heading() {
    // A tiny hand-written HTML file opens and yields the expected first heading.
    let path = temp_path("small.htm");
    std::fs::write(&path, b"<h1>Hello HTML</h1><p>body</p>").unwrap();

    let (mut state, mut wp, fmt) = open_document(&path).unwrap();
    assert_eq!(fmt, WpFormat::Html, ".htm also opens as HTML");
    wp.sync(&mut state.doc, &state.root);

    let snap = qdoc::model::materialize(&state.doc, &wp_containers(&wp, &state));
    assert_eq!(snap.blocks[0].kind, "heading");
    assert_eq!(snap.blocks[0].text, "Hello HTML");

    std::fs::remove_file(&path).ok();
}

/// The word processor binds the canonical model at `state.root`; resolve its
/// container ids the same way `open_document` did so we can materialize.
fn wp_containers(_wp: &WordProcessorWidget, state: &ui::State) -> qdoc::Containers {
    qdoc::model::open(&state.doc, &state.root).expect("document is bound at root")
}
