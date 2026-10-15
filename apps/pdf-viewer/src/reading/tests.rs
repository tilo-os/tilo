#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;

use serde_json::json;
use tilo_content::ContentStore;
use ui::tilo_resource::Effect as _;

use super::*;
use crate::actions::PdfEffect;
use crate::resource::{PdfViewerShell, PinnedSource};

/// A PDF of `pages`, each a list of lines set in Helvetica, titled `title`.
fn pdf(title: &str, pages: &[Vec<String>]) -> Vec<u8> {
    let escape = |text: &str| {
        text.replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)")
    };
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            (0..pages.len())
                .map(|i| format!("{} 0 R", 4 + 2 * i))
                .collect::<Vec<_>>()
                .join(" "),
            pages.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
    ];
    for (i, lines) in pages.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * i
        ));
        let mut stream = String::from("BT /F1 7 Tf 11 TL 36 756 Td");
        for line in lines {
            write!(stream, " ({}) Tj T*", escape(line)).unwrap();
        }
        stream.push_str(" ET");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    objects.push(format!("<< /Title ({}) /Author (Ada) >>", escape(title)));
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info {} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1,
            objects.len()
        )
        .as_bytes(),
    );
    out
}

fn lines(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// The markup of a record pinned to `bytes`, kept in a new content store.
fn pinned(bytes: &[u8]) -> (PdfMarkup, ContentStore) {
    let content = ContentStore::memory();
    let hash = content.put(bytes).unwrap();
    let markup = PdfMarkup {
        source: Some(PinnedSource {
            hash,
            len: bytes.len() as u64,
            name: "inventory.pdf".into(),
        }),
        ..PdfMarkup::default()
    };
    (markup, content)
}

fn three_pages() -> Vec<u8> {
    pdf(
        "Lamp inventory",
        &[
            lines(&["Lamps in the hall"]),
            lines(&["Lamps in the attic"]),
            lines(&["Lamps in the cellar"]),
        ],
    )
}

#[test]
fn an_agent_reads_every_page_of_a_short_pdf_with_its_facts() {
    let (markup, content) = pinned(&three_pages());
    let read = read_text(&markup, Some(&content), None, None, None).unwrap();
    assert_eq!(read["pageCount"], 3, "{read}");
    assert_eq!(read["name"], "inventory.pdf");
    assert_eq!(read["title"], "Lamp inventory");
    assert_eq!(read["author"], "Ada");
    let pages = read["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 3, "{read}");
    assert_eq!(pages[1]["page"], 2);
    assert!(
        pages[1]["text"]
            .as_str()
            .unwrap()
            .contains("Lamps in the attic"),
        "{read}"
    );
    assert!(
        read["next"].is_null(),
        "the whole document was read: {read}"
    );
}

#[test]
fn an_agent_reads_a_pdf_a_page_at_a_time() {
    let (markup, content) = pinned(&three_pages());
    let read = read_text(&markup, Some(&content), Some(2), Some(1), None).unwrap();
    let pages = read["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 1);
    assert!(pages[0]["text"].as_str().unwrap().contains("attic"));
    assert_eq!(read["next"], json!({"page": 3, "offset": 0}));

    let beyond = read_text(&markup, Some(&content), Some(4), None, None).unwrap_err();
    assert!(beyond.contains("3 pages"), "{beyond}");
}

#[test]
fn a_long_page_is_cut_and_read_on_from_where_it_stopped() {
    let line = |n: usize| {
        format!(
            "Line {n:02} of the long page, every lamp listed in order, with its room and its wattage."
        )
    };
    let long: Vec<String> = (0..64).map(line).collect();
    let whole: usize = long.iter().map(|l| l.len() + 1).sum();
    assert!(whole > MOST_CHARS, "the page is longer than one read");
    let (markup, content) = pinned(&pdf("Long", &[long, lines(&["The short page"])]));

    let first = read_text(&markup, Some(&content), None, None, None).unwrap();
    let pages = first["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 1, "a cut page ends the read: {first}");
    let text = pages[0]["text"].as_str().unwrap();
    assert_eq!(text.chars().count(), MOST_CHARS);
    let next = &first["next"];
    assert_eq!(next["page"], 1);
    let offset = usize::try_from(next["offset"].as_u64().unwrap()).unwrap();
    assert_eq!(offset, MOST_CHARS);

    let rest = read_text(&markup, Some(&content), Some(1), None, Some(offset)).unwrap();
    let pages = rest["pages"].as_array().unwrap();
    assert_eq!(pages[0]["offset"], offset);
    assert!(
        pages[0]["text"].as_str().unwrap().contains("Line 63"),
        "{rest}"
    );
    assert!(
        pages[1]["text"].as_str().unwrap().contains("short page"),
        "{rest}"
    );
    assert!(rest["next"].is_null());
}

#[test]
fn the_reader_page_order_is_what_an_agent_reads() {
    let (mut markup, content) = pinned(&three_pages());
    let entry = |id: &str, source_page| PagePlanEntry {
        id: id.into(),
        source_page,
        imported: None,
        rotation: 0,
    };
    markup.pages = vec![entry("c", 3), entry("a", 1)];
    let read = read_text(&markup, Some(&content), None, None, None).unwrap();
    assert_eq!(read["pageCount"], 2);
    let pages = read["pages"].as_array().unwrap();
    assert!(pages[0]["text"].as_str().unwrap().contains("cellar"));
    assert!(pages[1]["text"].as_str().unwrap().contains("hall"));
}

#[test]
fn a_record_without_a_pdf_says_how_to_open_one() {
    let error = read_text(
        &PdfMarkup::default(),
        Some(&ContentStore::memory()),
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(error.contains("import-pdf-content"), "{error}");
}

#[test]
fn reading_text_is_a_view_an_agent_may_run() {
    let spec = PdfEffect::specs()
        .into_iter()
        .find(|spec| spec.id == "read-text")
        .expect("read-text is published");
    assert_eq!(spec.kind, tilo_actions::OperationKind::View);
    assert!(matches!(
        spec.contract.result,
        tilo_actions::ResultSpec::Json { .. }
    ));
    assert!(
        !spec
            .contract
            .tags
            .iter()
            .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY),
        "it needs no device of its own"
    );
    assert!(tilo_actions::PermissionRequirement::Read.covers(spec.contract.permission));
}

#[test]
fn an_agent_opens_a_pdf_the_room_holds_and_reads_it() {
    let mut app = ui::framework::testing::TestApp::<PdfViewerShell>::new().unwrap();
    let bytes = three_pages();
    let hash = app.content().put(&bytes).unwrap();
    app.call(
        "import-pdf-content",
        json!({"content": format!("tilo.content:{hash}"), "name": "inventory.pdf"}),
    )
    .expect("the PDF is opened from content");
    app.settle();
    let source = app.state().source.expect("the record pins the PDF");
    assert_eq!(source.hash, hash);
    assert_eq!(source.name, "inventory.pdf");

    let output = app
        .effect(&PdfEffect::ReadText {
            page: Some(3),
            pages: None,
            offset: None,
        })
        .expect("read-text answers");
    let read = output.value.expect("a value");
    assert!(
        read["pages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cellar"),
        "{read}"
    );

    // Opening it again changes nothing; another PDF is refused.
    app.call("import-pdf-content", json!({"content": hash}))
        .expect("the same PDF opens again");
    let other = app
        .content()
        .put(&pdf("Other", &[lines(&["Another file"])]))
        .unwrap();
    let refused = app
        .call("import-pdf-content", json!({"content": other}))
        .unwrap_err();
    assert!(
        refused.to_string().contains("already holds inventory.pdf"),
        "{refused}"
    );
    // Bytes that are not a PDF are refused by name.
    let text = app.content().put(b"not a pdf").unwrap();
    let mut fresh = ui::framework::testing::TestApp::<PdfViewerShell>::new().unwrap();
    let text_hash = fresh.content().put(b"not a pdf").unwrap();
    assert_eq!(text, text_hash);
    let refused = fresh
        .call("import-pdf-content", json!({"content": text_hash}))
        .unwrap_err();
    assert!(
        refused.to_string().contains("not a readable PDF"),
        "{refused}"
    );
}
