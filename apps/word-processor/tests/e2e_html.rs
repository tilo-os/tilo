#![allow(
    clippy::unwrap_used,
    clippy::string_slice,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;

use word_processor::WpFormat;

use crate::support::{launched, snapshot};

fn temp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wp-html-e2e-{}-{}", std::process::id(), name));
    p
}

fn render_doc_to_html() -> String {
    let mut doc = automerge::AutoCommit::new();
    let c = word_processor::model::init(&mut doc).unwrap();
    let mut ids = tilo_doc::IdGen::new(doc.get_actor().to_hex_string());
    {
        let mut dm = tilo_doc::DocMut {
            doc: &mut doc,
            c: &c,
            ids: &mut ids,
        };
        let h = dm.insert_block(None, "heading");
        dm.set_attr(&h, "level", 1_i64);
        dm.splice_text(&h, 0, 0, "Report");

        let p = dm.insert_block(Some(&h), "paragraph");
        dm.splice_text(&p, 0, 0, "See bold and a link here");
        dm.add_mark(&p, 4, 8, "strong", true); // "bold"
        dm.add_mark(&p, 13, 19, "link", "https://tilo.so/"); // "a link"
    }
    let snap = tilo_doc::model::materialize(&doc, &c);
    tilo_doc_codecs::export_html::render_html(
        &snap,
        tilo_doc_codecs::export_html::HtmlOptions::default(),
        &tilo_content::NoContent,
    )
}

#[test]
fn html_opens_with_headings_bold_and_links_recovered() {
    {
        let html = render_doc_to_html();
        let path = temp_path("doc.html");
        std::fs::write(&path, html.as_bytes()).unwrap();

        assert_eq!(
            WpFormat::from_path(&path),
            WpFormat::Html,
            "extension picks the HTML format"
        );
        let snap = snapshot(&launched(&path));

        let first = &snap.blocks[0];
        assert_eq!(first.kind, "heading", "first block is a heading: {snap:?}");
        assert_eq!(first.text, "Report");
        assert_eq!(first.attrs.get("level"), Some(&tilo_doc::AttrVal::Int(1)));

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
            tilo_doc::document::MarkValue::Str("https://tilo.so/".into())
        );
        assert_eq!(&p.text[link.start..link.end], "a link", "{:?}", p.marks);

        std::fs::remove_file(&path).ok();
    }

    {
        let path = temp_path("small.htm");
        std::fs::write(&path, b"<h1>Hello HTML</h1><p>body</p>").unwrap();

        assert_eq!(
            WpFormat::from_path(&path),
            WpFormat::Html,
            ".htm also opens as HTML"
        );
        let snap = snapshot(&launched(&path));
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Hello HTML");

        std::fs::remove_file(&path).ok();
    }
}
