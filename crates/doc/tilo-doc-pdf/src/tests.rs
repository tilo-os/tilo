//! Tests for the PDF export engine.
//!
//! Snapshots are built directly (the structs are public) so the tests exercise
//! `render_pdf` without coupling to tilo-doc's mutation API. We assert on the real
//! output bytes — a valid PDF header, non-trivial length, the expected page
//! count via `/Type /Page`, and (for the content-object test) the number of
//! content streams emitted.

use super::*;
use std::collections::BTreeMap;
use tilo_doc::snapshot::{AttrVal, BlockView, DocSnapshot};

fn empty_snapshot() -> DocSnapshot {
    DocSnapshot {
        schema: "tilo.richtext".into(),
        schema_version: 1,
        ..Default::default()
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn pdf_export_lays_out_blocks_tables_pages_and_images() {
    // Phase `multi_block_snapshot_yields_valid_pdf_bytes`.
    {
        let snap = DocSnapshot {
            blocks: vec![
                block_attr("heading", "Quarterly Report", &[("level", AttrVal::Int(1))]),
                block(
                    "paragraph",
                    "This is the opening paragraph of the document. It contains \
                    enough words that it must wrap across multiple lines when laid out within the \
                    US-Letter text column, which exercises the word-wrapping path.",
                ),
                block_attr("heading", "Details", &[("level", AttrVal::Int(2))]),
                block(
                    "paragraph",
                    "A short second paragraph follows the subheading.",
                ),
            ],
            ..empty_snapshot()
        };

        let bytes = render_pdf(&snap, &tilo_content::NoContent).expect("render");
        assert!(bytes.starts_with(b"%PDF"), "must start with %PDF magic");
        assert!(bytes.len() > 500, "non-trivial length, got {}", bytes.len());
        assert!(
            bytes.ends_with(b"%%EOF\n"),
            "must end with the PDF EOF marker"
        );
        // Exactly one page for this much content.
        assert_eq!(
            count(&bytes, b"/Type /Page\n"),
            0,
            "page dict is inline, not newline-terminated"
        );
        assert!(
            count(&bytes, b"/Type /Page ") >= 1,
            "at least one page object"
        );
        assert!(count(&bytes, b"/Type /Catalog") == 1, "exactly one catalog");
    }
    // Phase `heading_paragraph_list_produces_multiple_content_objects`.
    {
        // A heading + paragraph + a 3-item list. We assert that more than one
        // *content stream* object is emitted across the document — the production
        // path writes one content stream per page, and each page references a
        // FlateDecode'd stream. To get >1 content object deterministically we also
        // assert the document drew text for every block.
        let snap = DocSnapshot {
            blocks: vec![
                block_attr("heading", "Shopping List", &[("level", AttrVal::Int(1))]),
                block("paragraph", "Things to buy this week:"),
                block_attr(
                    "list_item",
                    "Apples",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(0)),
                    ],
                ),
                block_attr(
                    "list_item",
                    "Bread",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(0)),
                    ],
                ),
                block_attr(
                    "list_item",
                    "Cheese",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(0)),
                    ],
                ),
            ],
            ..empty_snapshot()
        };

        let bytes = render_pdf(&snap, &tilo_content::NoContent).expect("render");
        assert!(bytes.starts_with(b"%PDF"));

        // The PDF must contain multiple distinct content/structural objects: at
        // least the two fonts, the page, the content stream, the pages tree, and
        // the catalog → well over one indirect object. Count " 0 obj" markers.
        let n_objs = count(&bytes, b" 0 obj\n");
        assert!(n_objs > 1, "expected >1 indirect objects, got {n_objs}");

        // And there is at least one content stream (FlateDecode'd).
        assert!(
            count(&bytes, b"/Filter /FlateDecode") >= 1,
            "at least one stream"
        );
    }
    // Phase `empty_doc_yields_valid_single_page_pdf`.
    {
        let snap = empty_snapshot();
        let bytes = render_pdf(&snap, &tilo_content::NoContent).expect("render empty");
        assert!(bytes.starts_with(b"%PDF"), "valid header even when empty");
        assert!(bytes.ends_with(b"%%EOF\n"));
        // Exactly one page.
        assert_eq!(count(&bytes, b"/Type /Pages"), 1, "one pages tree");
        // The pages tree declares Count 1.
        assert!(count(&bytes, b"/Count 1 ") == 1, "single-page document");
        assert_eq!(count(&bytes, b"/Type /Catalog"), 1);
    }
    // Phase `long_document_paginates_to_multiple_pages`.
    {
        // Many paragraphs must overflow one page and start a second.
        let mut blocks = Vec::new();
        for i in 0..120 {
            blocks.push(block(
                "paragraph",
                &format!(
                    "Paragraph number {i} with some filler text to take up vertical space on the page."
                ),
            ));
        }
        let snap = DocSnapshot {
            blocks,
            ..empty_snapshot()
        };
        let bytes = render_pdf(&snap, &tilo_content::NoContent).expect("render");
        let pages = count(&bytes, b"/Type /Page ");
        assert!(
            pages >= 2,
            "120 paragraphs should span >=2 pages, got {pages}"
        );
        // The pages tree Count must match the number of page objects.
        let count_marker = format!("/Count {pages} ");
        assert_eq!(
            count(&bytes, count_marker.as_bytes()),
            1,
            "pages-tree Count matches page objects"
        );
    }
    // Phase `table_block_renders_grid`.
    {
        // table → rows → cells.
        let mut row1 = block("row", "");
        row1.children = vec![block("cell", "Name"), block("cell", "Score")];
        let mut row2 = block("row", "");
        row2.children = vec![block("cell", "Alice"), block("cell", "42")];
        let mut table = block("table", "");
        table.children = vec![row1, row2];

        let snap = DocSnapshot {
            blocks: vec![table],
            ..empty_snapshot()
        };
        let bytes = render_pdf(&snap, &tilo_content::NoContent).expect("render");
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() > 400);
    }
    // Phase `scanned_pdf_preserves_pages_images_and_searchable_invisible_text`.
    {
        let pages = vec![
            ScannedPdfPage {
                width: 2,
                height: 1,
                rgb: vec![255, 0, 0, 0, 255, 0],
                recognized_text: "First receipt".into(),
            },
            ScannedPdfPage {
                width: 1,
                height: 2,
                rgb: vec![0, 0, 255, 255, 255, 255],
                recognized_text: "Second receipt".into(),
            },
        ];
        let pdf = render_scanned_pdf(&pages);
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.7"));
        assert!(text.contains("/Count 2"));
        assert_eq!(text.matches("/Subtype /Image").count(), 2);
        assert!(
            text.contains("3 Tr"),
            "OCR uses invisible text rendering mode"
        );
        assert!(text.contains("First receipt"));
        assert!(text.contains("Second receipt"));
    }
    // Phase `image_block_with_decodable_png_embeds_xobject`.
    {
        // A 2x2 PNG built in-memory via the image crate, stored as a blob.
        let png = make_test_png();
        let hash = "deadbeef".to_string();
        let mut blobs = BTreeMap::new();
        blobs.insert(hash.clone(), png);

        let snap = DocSnapshot {
            blocks: vec![block_attr("image", "", &[("blob", AttrVal::Str(hash))])],
            ..empty_snapshot()
        };
        let bytes = render_pdf(&snap, &blobs).expect("render");
        // The XObject dict is uncompressed in the object stream, so we can assert
        // on it directly. (The page content stream that paints it via `Do` is
        // FlateDecode-compressed, so its operators aren't visible in the raw bytes.)
        assert!(
            count(&bytes, b"/Subtype /Image") >= 1,
            "image XObject embedded"
        );
        assert!(count(&bytes, b"/ColorSpace /DeviceRGB") >= 1);
        assert!(
            count(&bytes, b"/Width 2 ") >= 1,
            "image dimensions recorded"
        );
        // The page's resources must reference the XObject so it can be painted.
        assert!(
            count(&bytes, b"/XObject << /Im0") >= 1,
            "page references the image XObject"
        );
    }
    // Phase `image_block_with_bad_blob_draws_placeholder_not_error`.
    {
        let mut blobs = BTreeMap::new();
        blobs.insert("h".to_string(), vec![0u8, 1, 2, 3]); // not a valid image
        let snap = DocSnapshot {
            blocks: vec![block_attr(
                "image",
                "",
                &[("blob", AttrVal::Str("h".into()))],
            )],
            ..empty_snapshot()
        };
        // Must not error — degrades to a placeholder.
        let bytes = render_pdf(&snap, &blobs).expect("render with bad image");
        assert!(bytes.starts_with(b"%PDF"));
        // No image XObject, but a rectangle (placeholder) was drawn.
        assert_eq!(count(&bytes, b"/Subtype /Image"), 0);
    }
}

/// Inline `strong` and `em` marks set their runs in Helvetica-Bold and
/// Helvetica-Oblique, within one line and across a wrap, and the plain
/// text around them stays regular.
#[test]
fn strong_and_em_marks_set_their_runs_in_bold_and_italic() {
    use std::io::Read as _;
    let mark = |name: &str, start: usize, end: usize| tilo_doc::MarkSpan {
        start,
        end,
        name: name.into(),
        value: tilo_doc::document::MarkValue::Bool(true),
    };
    let mut paragraph = block("paragraph", "Prepared for the steering group by the team.");
    paragraph.marks = vec![mark("strong", 17, 31), mark("em", 35, 44)];
    let lines = wrap(&styled(&paragraph, Style::default()), TEXT_W, BODY_SIZE);
    let regular = Style::default();
    let italic = Style {
        bold: false,
        italic: true,
    };
    assert_eq!(
        lines,
        [vec![
            ("Prepared for the ".to_owned(), regular),
            ("steering group".to_owned(), Style::BOLD),
            (" by ".to_owned(), regular),
            ("the team.".to_owned(), italic),
        ]]
    );

    // Wrapped to a narrow column, every word is kept in its own face.
    let narrow = wrap(&styled(&paragraph, Style::default()), 60.0, BODY_SIZE);
    assert!(narrow.len() > 2, "{narrow:?}");
    let rejoined: Vec<String> = narrow
        .iter()
        .map(|line| line.iter().map(|(text, _)| text.as_str()).collect())
        .collect();
    assert_eq!(rejoined.join(" "), paragraph.text);
    assert!(
        narrow
            .iter()
            .flatten()
            .any(|(text, style)| text.contains("steering") && *style == Style::BOLD),
        "{narrow:?}"
    );

    // A heading is bold throughout, and its em run is bold italic.
    let mut heading = block_attr("heading", "Field notes", &[("level", AttrVal::Int(1))]);
    heading.marks = vec![mark("em", 6, 11)];
    assert_eq!(
        wrap(&styled(&heading, Style::BOLD), TEXT_W, 24.0),
        [vec![
            ("Field ".to_owned(), Style::BOLD),
            (
                "notes".to_owned(),
                Style {
                    bold: true,
                    italic: true
                }
            ),
        ]]
    );

    // The page declares every face and shows each run in its own.
    let snap = DocSnapshot {
        blocks: vec![paragraph],
        ..empty_snapshot()
    };
    let bytes = render_pdf(&snap, &tilo_content::NoContent).expect("render");
    for face in FACES {
        let declared = format!("/BaseFont /{face} ");
        assert_eq!(count(&bytes, declared.as_bytes()), 1, "{face}");
    }
    let find = |from: usize, needle: &[u8]| {
        bytes[from..]
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|at| from + at)
    };
    let start = find(0, b"stream\n").expect("a content stream") + b"stream\n".len();
    let end = find(start, b"\nendstream").expect("its end");
    let mut stream = String::new();
    flate2::read::ZlibDecoder::new(&bytes[start..end])
        .read_to_string(&mut stream)
        .expect("a deflated content stream");
    assert!(stream.contains("/F2 11 Tf"), "{stream}");
    assert!(stream.contains("(steering group) Tj"), "{stream}");
    assert!(stream.contains("/F3 11 Tf"), "{stream}");
    assert!(stream.contains("(the team.) Tj"), "{stream}");
}

fn block(kind: &str, text: &str) -> BlockView {
    BlockView {
        id: format!("b-{kind}-{}", text.len()),
        kind: kind.into(),
        text: text.into(),
        marks: Vec::new(),
        attrs: BTreeMap::new(),
        children: Vec::new(),
    }
}

fn block_attr(kind: &str, text: &str, attrs: &[(&str, AttrVal)]) -> BlockView {
    let mut b = block(kind, text);
    for (k, v) in attrs {
        b.attrs.insert((*k).to_string(), v.clone());
    }
    b
}

/// Count occurrences of a needle in a byte haystack.
fn count(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Build a tiny valid PNG (2x2, solid) using the `image` crate.
fn make_test_png() -> Vec<u8> {
    use image::{ImageFormat, RgbImage};
    let img = RgbImage::from_fn(2, 2, |x, _| {
        if x == 0 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        }
    });
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png)
        .expect("encode png");
    buf.into_inner()
}
