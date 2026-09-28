//! Tests for the RTF export engine.
//!
//! Snapshots are built directly (the `DocSnapshot`/`BlockView`/`MarkSpan`
//! structs are public) so the tests exercise `render_rtf` without coupling to
//! qdoc's mutation API. We assert on the real emitted string — the production
//! path — for control words, color-table registration, escaping, and the
//! balanced-group (well-formedness) guarantee.

use super::*;
use std::collections::BTreeMap;
use tilo_doc::MarkSpan;
use tilo_doc::document::MarkValue;
use tilo_doc::snapshot::{AttrVal, BlockView, DocSnapshot};

fn empty_snapshot() -> DocSnapshot {
    DocSnapshot {
        schema: "tilo.richtext".into(),
        schema_version: 1,
        ..Default::default()
    }
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

fn mark(start: usize, end: usize, name: &str, value: MarkValue) -> MarkSpan {
    MarkSpan {
        start,
        end,
        name: name.into(),
        value,
    }
}

fn count(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    hay.matches(needle).count()
}

/// Assert every `{` is matched by a `}` and groups never go negative — i.e. the
/// document is a well-formed nest of balanced groups. Backslash-escaped braces
/// (`\{` / `\}`) are literal text, not group delimiters, and must be excluded.
fn assert_balanced(rtf: &str) {
    let bytes = rtf.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                // Skip the escaped next byte (covers \{ \} \\ and control words'
                // leading char — we only care that braces after a backslash are
                // not counted).
                i = i.saturating_add(2);
                continue;
            }
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                // `depth` is signed, so `saturating_sub` only clamps at
                // `i32::MIN` — light-years past any real brace count — and
                // the assert right below still sees the negative depth a
                // stray closing brace produces.
                depth = depth.saturating_sub(1);
                assert!(
                    depth >= 0,
                    "unbalanced: closing brace with no open at byte {i}\n{rtf}"
                );
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    assert_eq!(depth, 0, "unbalanced braces (depth {depth} at end):\n{rtf}");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn rtf_export_block_structure() {
    // Phase `heading_with_bold_run_yields_rtf_header_and_bold`.
    {
        // A heading whose first word is bold.
        let mut heading = block_attr("heading", "Bold title", &[("level", AttrVal::Int(1))]);
        heading
            .marks
            .push(mark(0, 4, "strong", MarkValue::Bool(true)));

        let snap = DocSnapshot {
            blocks: vec![heading],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        // RTF header.
        assert!(rtf.starts_with("{\\rtf1"), "starts with RTF header:\n{rtf}");
        assert!(rtf.contains("\\ansi"), "ansi declared:\n{rtf}");
        assert!(rtf.contains("\\fonttbl"), "font table present:\n{rtf}");
        // A bold control word appears (heading paragraph is \b, and the run is \b).
        assert!(rtf.contains("\\b"), "bold control word present:\n{rtf}");
        // Larger heading font (H1 = 24pt → \fs48).
        assert!(rtf.contains("\\fs48"), "H1 renders at \\fs48:\n{rtf}");
        // The text survives.
        assert!(rtf.contains("Bold"), "heading text present:\n{rtf}");
        assert_balanced(&rtf);
    }
    // Phase `three_item_list_yields_three_pars_and_bullets`.
    {
        let snap = DocSnapshot {
            blocks: vec![
                block_attr("list_item", "One", &[("level", AttrVal::Int(0))]),
                block_attr("list_item", "Two", &[("level", AttrVal::Int(0))]),
                block_attr("list_item", "Three", &[("level", AttrVal::Int(0))]),
            ],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        assert_eq!(count(&rtf, "\\par}"), 3, "three paragraphs:\n{rtf}");
        assert_eq!(count(&rtf, "\\bullet"), 3, "three bullets:\n{rtf}");
        assert!(
            rtf.contains("One") && rtf.contains("Two") && rtf.contains("Three"),
            "items present:\n{rtf}"
        );
        assert_balanced(&rtf);
    }
    // Phase `ordered_list_yields_numbered_markers`.
    {
        // Markers are literal text taken from each item's `index` attr (RTF list
        // tables are out of scope); the snapshot supplies the per-item number.
        let snap = DocSnapshot {
            blocks: vec![
                block_attr(
                    "list_item",
                    "First",
                    &[
                        ("ordered", AttrVal::Bool(true)),
                        ("level", AttrVal::Int(0)),
                        ("index", AttrVal::Int(1)),
                    ],
                ),
                block_attr(
                    "list_item",
                    "Second",
                    &[
                        ("ordered", AttrVal::Bool(true)),
                        ("level", AttrVal::Int(0)),
                        ("index", AttrVal::Int(2)),
                    ],
                ),
            ],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        // Numbered, not bulleted.
        assert_eq!(
            count(&rtf, "\\bullet"),
            0,
            "ordered list has no bullets:\n{rtf}"
        );
        assert!(rtf.contains("1.\\tab "), "first item numbered:\n{rtf}");
        assert!(rtf.contains("2.\\tab "), "second item numbered:\n{rtf}");
        assert_balanced(&rtf);
    }
    // Phase `empty_document_is_well_formed`.
    {
        let rtf = render_rtf(&empty_snapshot());
        assert!(rtf.starts_with("{\\rtf1"));
        assert!(
            rtf.contains("{\\colortbl;}"),
            "empty color table still emitted:\n{rtf}"
        );
        assert_balanced(&rtf);
    }
    // Phase `quote_is_indented`.
    {
        let snap = DocSnapshot {
            blocks: vec![block("quote", "wise words")],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);
        assert!(rtf.contains("\\li"), "quote carries a left indent:\n{rtf}");
        assert!(rtf.contains("\\i"), "quote is italic:\n{rtf}");
        assert_balanced(&rtf);
    }
    // Phase `table_emits_rows_and_cells`.
    {
        // A 2x2 table as rows-of-cells.
        let mut r1 = block("table_row", "");
        r1.children = vec![block("cell", "A"), block("cell", "B")];
        let mut r2 = block("table_row", "");
        r2.children = vec![block("cell", "C"), block("cell", "D")];
        let mut table = block("table", "");
        table.children = vec![r1, r2];

        let snap = DocSnapshot {
            blocks: vec![table],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        assert_eq!(count(&rtf, "\\row"), 2, "two rows:\n{rtf}");
        assert_eq!(count(&rtf, "\\cell "), 4, "four cells:\n{rtf}");
        assert!(rtf.contains("\\cellx"), "cell boundaries declared:\n{rtf}");
        assert!(
            rtf.contains('A') && rtf.contains('D'),
            "cell contents present:\n{rtf}"
        );
        assert_balanced(&rtf);
    }
}

#[test]
fn rtf_export_escapes_specials_and_non_ascii() {
    // Phase `rtf_specials_are_escaped`.
    {
        // Text containing every RTF special.
        let snap = DocSnapshot {
            blocks: vec![block("paragraph", "a{b}c\\d")],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        // The specials are backslash-escaped in the body.
        assert!(rtf.contains("a\\{b\\}c\\\\d"), "specials escaped:\n{rtf}");
        // And the document is still balanced (the escaped braces don't count).
        assert_balanced(&rtf);
    }
    // Phase `non_ascii_emits_unicode_escape`.
    {
        // "café" → the é is non-ASCII (U+00E9 = 233).
        let snap = DocSnapshot {
            blocks: vec![block("paragraph", "café")],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);
        assert!(rtf.contains("caf\\u233?"), "é emitted as \\u233?:\n{rtf}");
        // The ASCII prefix stays literal.
        assert!(rtf.contains("caf"));
        assert_balanced(&rtf);
    }
    // Phase `multibyte_text_uses_character_mark_offsets`.
    {
        let mut para = block("paragraph", "héllo");
        para.marks.push(mark(0, 4, "strong", MarkValue::Bool(true)));
        let rtf = render_rtf(&DocSnapshot {
            blocks: vec![para],
            ..empty_snapshot()
        });

        assert!(
            rtf.contains("{\\b h\\u233?ll}{o}"),
            "bold covers four Unicode scalar values, not four bytes:\n{rtf}"
        );
        assert_balanced(&rtf);
    }
}

#[test]
fn rtf_export_marks_map_to_control_words_and_tables() {
    // Phase `colored_run_registers_color_table_entry_and_cf`.
    {
        let mut para = block("paragraph", "red text");
        para.marks
            .push(mark(0, 3, "color", MarkValue::Str("#ff0000".into())));

        let snap = DocSnapshot {
            blocks: vec![para],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        // The color table carries the registered color.
        assert!(rtf.contains("{\\colortbl;"), "color table present:\n{rtf}");
        assert!(
            rtf.contains("\\red255\\green0\\blue0;"),
            "red registered in color table:\n{rtf}"
        );
        // The run references it (index 1 — index 0 is the reserved auto color).
        assert!(
            rtf.contains("\\cf1"),
            "run references color index 1:\n{rtf}"
        );
        assert_balanced(&rtf);
    }
    // Phase `hyperlink_mark_emits_field`.
    {
        let mut para = block("paragraph", "click here");
        para.marks.push(mark(
            0,
            10,
            "href",
            MarkValue::Str("https://example.com".into()),
        ));

        let snap = DocSnapshot {
            blocks: vec![para],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        assert!(
            rtf.contains("{\\field{\\*\\fldinst HYPERLINK \"https://example.com\"}"),
            "hyperlink field:\n{rtf}"
        );
        assert!(rtf.contains("\\fldrslt"), "field result group:\n{rtf}");
        assert!(rtf.contains("click here"), "link text present:\n{rtf}");
        assert_balanced(&rtf);
    }
    // Phase `font_and_size_marks_map_to_control_words`.
    {
        let mut para = block("paragraph", "mono big");
        para.marks
            .push(mark(0, 4, "font", MarkValue::Str("mono".into())));
        para.marks.push(mark(0, 4, "size", MarkValue::Int(18)));

        let snap = DocSnapshot {
            blocks: vec![para],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        // mono → \f2 (FONT_MONO); size 18pt → \fs36 (half-points).
        assert!(rtf.contains("\\f2"), "mono font index:\n{rtf}");
        assert!(rtf.contains("\\fs36"), "18pt → \\fs36:\n{rtf}");
        assert_balanced(&rtf);
    }
    // Phase `distinct_colors_get_distinct_indices`.
    {
        let mut p1 = block("paragraph", "red");
        p1.marks
            .push(mark(0, 3, "color", MarkValue::Str("#ff0000".into())));
        let mut p2 = block("paragraph", "blue");
        p2.marks
            .push(mark(0, 4, "color", MarkValue::Str("#0000ff".into())));
        // A second red reuses index 1.
        let mut p3 = block("paragraph", "red again");
        p3.marks
            .push(mark(0, 3, "color", MarkValue::Str("#ff0000".into())));

        let snap = DocSnapshot {
            blocks: vec![p1, p2, p3],
            ..empty_snapshot()
        };
        let rtf = render_rtf(&snap);

        assert!(
            rtf.contains("\\red255\\green0\\blue0;"),
            "red registered:\n{rtf}"
        );
        assert!(
            rtf.contains("\\red0\\green0\\blue255;"),
            "blue registered:\n{rtf}"
        );
        // Two distinct colors → exactly two entries in the table (count the `;`
        // separators after the reserved auto entry by counting color triples).
        assert_eq!(
            count(&rtf, "\\green"),
            2,
            "exactly two color-table entries:\n{rtf}"
        );
        assert!(
            rtf.contains("\\cf1") && rtf.contains("\\cf2"),
            "both indices referenced:\n{rtf}"
        );
        assert_balanced(&rtf);
    }
}
