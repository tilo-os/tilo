//! Tests for the HTML export engine.
//!
//! Snapshots are built directly (the `DocSnapshot`/`BlockView`/`MarkSpan`
//! structs are public) so the tests exercise `render_html` without coupling to
//! qdoc's mutation API. We assert on the real emitted string — the production
//! path — for semantic tags, correct mark nesting, escaping, and the
//! no-injection guarantee.

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

/// Count occurrences of a substring.
fn count(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    hay.matches(needle).count()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn html_export_block_structure_images_and_scaffolding() {
    // Phase `heading_paragraph_bold_run_emits_h1_strong_and_escaped_text`.
    {
        // A heading, then a paragraph whose first word is bold and which contains a
        // character that MUST be escaped.
        let mut para = block("paragraph", "Bold & plain");
        // "Bold" = bytes 0..4 carries `strong`.
        para.marks.push(mark(0, 4, "strong", MarkValue::Bool(true)));

        let snap = DocSnapshot {
            blocks: vec![
                block_attr("heading", "Title", &[("level", AttrVal::Int(1))]),
                para,
            ],
            ..empty_snapshot()
        };

        let html = render_html(&snap, HtmlOptions::default(), &tilo_content::NoContent);

        assert!(
            html.contains("<h1>Title</h1>"),
            "heading level 1 → <h1>:\n{html}"
        );
        assert!(
            html.contains("<strong>Bold</strong>"),
            "bold run wrapped in <strong>:\n{html}"
        );
        // The `&` in the paragraph text is escaped.
        assert!(
            html.contains("Bold</strong> &amp; plain"),
            "ampersand escaped:\n{html}"
        );
        assert!(
            !html.contains(" & plain"),
            "raw ampersand must not appear:\n{html}"
        );
        // Full document scaffolding present.
        assert!(
            html.starts_with("<!doctype html>"),
            "full document:\n{html}"
        );
    }
    // Phase `heading_levels_map_to_h1_through_h6`.
    {
        for lvl in 1..=6 {
            let snap = DocSnapshot {
                blocks: vec![block_attr("heading", "H", &[("level", AttrVal::Int(lvl))])],
                ..empty_snapshot()
            };
            let html = render_html(
                &snap,
                HtmlOptions {
                    fragment: true,
                    ..Default::default()
                },
                &tilo_content::NoContent,
            );
            assert!(
                html.contains(&format!("<h{lvl}>H</h{lvl}>")),
                "level {lvl}:\n{html}"
            );
        }
        // Out-of-range clamps to h6 (>6) and h1 (<1).
        let snap = DocSnapshot {
            blocks: vec![block_attr("heading", "X", &[("level", AttrVal::Int(9))])],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(html.contains("<h6>X</h6>"), "level 9 clamps to h6:\n{html}");
    }
    // Phase `quote_renders_blockquote`.
    {
        let snap = DocSnapshot {
            blocks: vec![block("quote", "To be or not to be")],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(html.contains("<blockquote>"), "quote → blockquote:\n{html}");
        assert!(html.contains("To be or not to be"));
        assert!(html.contains("</blockquote>"));
    }
    // Phase `table_renders_tr_td_grid`.
    {
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
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert_eq!(count(&html, "<table"), 1);
        assert_eq!(count(&html, "<tr>"), 2);
        assert_eq!(count(&html, "<td>"), 4);
        assert!(html.contains(">Name</td>") && html.contains(">42</td>"));
    }
    // Phase `image_block_emits_img_with_data_uri`.
    {
        // A minimal PNG header so the MIME sniff picks image/png.
        // The trailing 0xfb 0xff exercise the `+`/`/` alphabet and `=` padding.
        let png = vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4, 0xfb, 0xff,
        ];
        let hash = "abc123".to_string();
        let mut blobs = BTreeMap::new();
        blobs.insert(hash.clone(), png);

        let snap = DocSnapshot {
            blocks: vec![block_attr(
                "image",
                "A cat",
                &[("blob", AttrVal::Str(hash))],
            )],
            ..empty_snapshot()
        };

        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &blobs,
        );

        assert!(
            html.contains("<img src=\"data:image"),
            "data URI img:\n{html}"
        );
        assert!(
            html.contains("<img src=\"data:image/png;base64,iVBORw0KGgoBAgME+/8=\""),
            "png mime + standard padded base64:\n{html}"
        );
        assert!(html.contains("alt=\"A cat\""), "alt text carried:\n{html}");
    }
    // Phase `missing_image_blob_emits_inert_img_without_error`.
    {
        let snap = DocSnapshot {
            blocks: vec![block_attr(
                "image",
                "lost",
                &[("blob", AttrVal::Str("nope".into()))],
            )],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(
            html.contains("<img src=\"\""),
            "inert img placeholder:\n{html}"
        );
        assert!(html.contains("alt=\"lost\""));
    }
    // Phase `styles_map_emits_css_classes_and_blocks_get_tagged`.
    {
        let mut fancy = BTreeMap::new();
        fancy.insert("color".to_string(), AttrVal::Str("#112233".into()));
        fancy.insert("size".to_string(), AttrVal::Float(20.0));
        let mut styles = BTreeMap::new();
        styles.insert("Fancy".to_string(), fancy);

        let snap = DocSnapshot {
            blocks: vec![block_attr(
                "paragraph",
                "styled para",
                &[("style", AttrVal::Str("Fancy".into()))],
            )],
            styles,
            ..empty_snapshot()
        };
        // Full doc so the <style> block is emitted.
        let html = render_html(&snap, HtmlOptions::default(), &tilo_content::NoContent);
        assert!(html.contains("<style>"), "style block present:\n{html}");
        assert!(
            html.contains(".qdoc-style-Fancy {"),
            "css class for the style:\n{html}"
        );
        assert!(
            html.contains("color: #112233;"),
            "color declaration:\n{html}"
        );
        assert!(
            html.contains("font-size: 20pt;"),
            "size declaration:\n{html}"
        );
        // The block references the class.
        assert!(
            html.contains("class=\"qdoc-style-Fancy\""),
            "block tagged with class:\n{html}"
        );
    }
    // Phase `empty_document_is_well_formed`.
    {
        let html = render_html(
            &empty_snapshot(),
            HtmlOptions::default(),
            &tilo_content::NoContent,
        );
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<body>") && html.contains("</body>"));
        assert!(html.trim_end().ends_with("</html>"));
    }
    // Phase `fragment_mode_omits_document_scaffolding`.
    {
        let snap = DocSnapshot {
            blocks: vec![block("paragraph", "hi")],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(
            !html.contains("<!doctype"),
            "no doctype in fragment:\n{html}"
        );
        assert!(!html.contains("<html"), "no html element:\n{html}");
        assert!(
            !html.contains("<style>"),
            "no style block in fragment:\n{html}"
        );
        assert!(html.contains("<p>hi</p>"), "just the body markup:\n{html}");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn html_export_lists_nest_and_number() {
    // Phase `three_item_ordered_list_yields_one_ol_with_three_li`.
    {
        let snap = DocSnapshot {
            blocks: vec![
                block_attr(
                    "list_item",
                    "One",
                    &[("ordered", AttrVal::Bool(true)), ("level", AttrVal::Int(0))],
                ),
                block_attr(
                    "list_item",
                    "Two",
                    &[("ordered", AttrVal::Bool(true)), ("level", AttrVal::Int(0))],
                ),
                block_attr(
                    "list_item",
                    "Three",
                    &[("ordered", AttrVal::Bool(true)), ("level", AttrVal::Int(0))],
                ),
            ],
            ..empty_snapshot()
        };

        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );

        assert_eq!(count(&html, "<ol"), 1, "exactly one <ol>:\n{html}");
        assert_eq!(count(&html, "</ol>"), 1, "exactly one </ol>:\n{html}");
        assert_eq!(count(&html, "<li"), 3, "three <li>:\n{html}");
        assert_eq!(count(&html, "</li>"), 3, "three </li> close:\n{html}");
        assert_eq!(count(&html, "<ul"), 0, "no bullet list:\n{html}");
        assert!(
            html.contains(">One</li>")
                && html.contains(">Two</li>")
                && html.contains(">Three</li>")
        );
    }
    // Phase `bullet_list_uses_ul`.
    {
        let snap = DocSnapshot {
            blocks: vec![
                block_attr(
                    "list_item",
                    "a",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(0)),
                    ],
                ),
                block_attr(
                    "list_item",
                    "b",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(0)),
                    ],
                ),
            ],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert_eq!(count(&html, "<ul>"), 1);
        assert_eq!(count(&html, "</ul>"), 1);
        assert_eq!(count(&html, "<li"), 2);
        assert_eq!(count(&html, "<ol"), 0);
    }
    // Phase `nested_list_levels_nest_correctly`.
    {
        // L0, L1, L1, L0 → an outer list with a sublist that opens and closes.
        let snap = DocSnapshot {
            blocks: vec![
                block_attr(
                    "list_item",
                    "top",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(0)),
                    ],
                ),
                block_attr(
                    "list_item",
                    "sub1",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(1)),
                    ],
                ),
                block_attr(
                    "list_item",
                    "sub2",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(1)),
                    ],
                ),
                block_attr(
                    "list_item",
                    "top2",
                    &[
                        ("ordered", AttrVal::Bool(false)),
                        ("level", AttrVal::Int(0)),
                    ],
                ),
            ],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        // Two <ul> open (outer + nested), two close — balanced.
        assert_eq!(count(&html, "<ul>"), 2, "outer + nested ul:\n{html}");
        assert_eq!(count(&html, "</ul>"), 2, "balanced close:\n{html}");
        assert_eq!(count(&html, "<li"), 4);
        // The nested items appear between the two top-level items in source order.
        let top = html.find("top<").unwrap();
        let sub1 = html.find("sub1<").unwrap();
        let top2 = html.find("top2<").unwrap();
        assert!(
            top < sub1 && sub1 < top2,
            "nesting preserves order:\n{html}"
        );
    }
    // Phase `ordered_list_num_format_sets_list_style_type`.
    {
        let snap = DocSnapshot {
            blocks: vec![block_attr(
                "list_item",
                "i",
                &[
                    ("ordered", AttrVal::Bool(true)),
                    ("level", AttrVal::Int(0)),
                    ("num_format", AttrVal::Str("lower-roman".into())),
                ],
            )],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(
            html.contains("list-style-type: lower-roman"),
            "roman numbering:\n{html}"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn html_export_escapes_and_neutralises_hostile_content() {
    // Phase `script_in_text_is_escaped_no_injection`.
    {
        let snap = DocSnapshot {
            blocks: vec![block("paragraph", "<script>alert('xss')</script>")],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );

        // The literal tag must NOT appear; only its escaped form.
        assert!(!html.contains("<script>"), "no raw <script> tag:\n{html}");
        assert!(
            html.contains("&lt;script&gt;"),
            "escaped script tag:\n{html}"
        );
        assert!(
            html.contains("&lt;/script&gt;"),
            "escaped closing tag:\n{html}"
        );
    }
    // Phase `link_mark_renders_anchor_with_escaped_href`.
    {
        let mut p = block("paragraph", "click here");
        p.marks.push(mark(
            0,
            10,
            "link",
            MarkValue::Str("https://example.com/?a=1&b=2".into()),
        ));
        let snap = DocSnapshot {
            blocks: vec![p],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(
            html.contains("<a href=\"https://example.com/?a=1&amp;b=2\">"),
            "anchor + escaped &:\n{html}"
        );
        assert!(html.contains("</a>"));
    }
    // Phase `javascript_href_is_neutralized`.
    {
        let mut p = block("paragraph", "evil");
        p.marks.push(mark(
            0,
            4,
            "link",
            MarkValue::Str("javascript:alert(1)".into()),
        ));
        let snap = DocSnapshot {
            blocks: vec![p],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(
            !html.contains("javascript:"),
            "javascript: scheme stripped:\n{html}"
        );
        assert!(html.contains("<a href=\"#\">"), "neutralized to #:\n{html}");
    }
    // Phase `css_injection_via_color_mark_is_sanitized`.
    {
        // A color value trying to break out of the style attribute.
        let mut p = block("paragraph", "x");
        p.marks.push(mark(
            0,
            1,
            "color",
            MarkValue::Str("red; } body { display:none".into()),
        ));
        let snap = DocSnapshot {
            blocks: vec![p],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        // Isolate the emitted style attribute value.
        let start = html.find("style=\"").expect("has style attr") + "style=\"".len();
        let attr = &html[start..start + html[start..].find('"').expect("closing quote")];
        // Exactly one declaration: the renderer appends a single trailing `;` to
        // close `color: <value>`, so the value itself must contribute NO extra `;`.
        // It also must not contain `{`/`}` (which would start a new rule block).
        assert_eq!(
            attr.matches(';').count(),
            1,
            "only the renderer's own terminator survives: {attr:?}\n{html}"
        );
        assert!(
            !attr.contains('{') && !attr.contains('}'),
            "no rule-block breakout: {attr:?}\n{html}"
        );
        // The injected second-property `;` that preceded ` } body {` is gone.
        assert!(
            !attr.contains("; }"),
            "no injected declaration boundary: {attr:?}\n{html}"
        );
    }
    // Phase `attribute_value_quotes_are_escaped_in_alt`.
    {
        // An image alt containing a double-quote must not break out of the attr.
        let png = vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0];
        let mut blobs = BTreeMap::new();
        blobs.insert("h".to_string(), png);
        let snap = DocSnapshot {
            blocks: vec![block_attr(
                "image",
                "a \"quoted\" caption",
                &[("blob", AttrVal::Str("h".into()))],
            )],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &blobs,
        );
        assert!(
            html.contains("alt=\"a &quot;quoted&quot; caption\""),
            "quotes escaped in attr:\n{html}"
        );
        // No raw quote that could terminate the attribute early.
        assert!(
            !html.contains("alt=\"a \"quoted"),
            "no attribute breakout:\n{html}"
        );
    }
}

#[test]
fn html_export_marks_split_runs_at_boundaries() {
    // Phase `all_boolean_marks_render_their_tags`.
    {
        let mut p = block("paragraph", "styled");
        p.marks.push(mark(0, 6, "strong", MarkValue::Bool(true)));
        p.marks.push(mark(0, 6, "em", MarkValue::Bool(true)));
        p.marks.push(mark(0, 6, "underline", MarkValue::Bool(true)));
        p.marks.push(mark(0, 6, "strike", MarkValue::Bool(true)));
        let snap = DocSnapshot {
            blocks: vec![p],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(html.contains("<strong>"), "strong:\n{html}");
        assert!(html.contains("<em>"), "em:\n{html}");
        assert!(html.contains("<u>"), "u:\n{html}");
        assert!(html.contains("<s>"), "s:\n{html}");
        // And the tags close.
        assert!(
            html.contains("</s>")
                && html.contains("</u>")
                && html.contains("</em>")
                && html.contains("</strong>")
        );
    }
    // Phase `partial_bold_run_splits_text_at_mark_boundary`.
    {
        // "Hello world" with bold only over "world" (bytes 6..11).
        let mut p = block("paragraph", "Hello world");
        p.marks.push(mark(6, 11, "strong", MarkValue::Bool(true)));
        let snap = DocSnapshot {
            blocks: vec![p],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(
            html.contains("Hello <strong>world</strong>"),
            "boundary split:\n{html}"
        );
    }
    // Phase `color_and_size_marks_emit_span_style`.
    {
        let mut p = block("paragraph", "rainbow");
        p.marks
            .push(mark(0, 7, "color", MarkValue::Str("#ff0000".into())));
        p.marks.push(mark(0, 7, "fontSize", MarkValue::Int(18)));
        let snap = DocSnapshot {
            blocks: vec![p],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(
            html.contains("<span style=\""),
            "span carrying style:\n{html}"
        );
        assert!(html.contains("color: #ff0000"), "color decl:\n{html}");
        assert!(html.contains("font-size: 18pt"), "size decl:\n{html}");
    }
    // Phase `multibyte_text_with_marks_splits_on_char_boundaries`.
    {
        // qdoc mark offsets count Unicode scalar values, not UTF-8 bytes.
        let text = "héllo";
        let mut p = block("paragraph", text);
        p.marks.push(mark(0, 4, "strong", MarkValue::Bool(true)));
        let snap = DocSnapshot {
            blocks: vec![p],
            ..empty_snapshot()
        };
        let html = render_html(
            &snap,
            HtmlOptions {
                fragment: true,
                ..Default::default()
            },
            &tilo_content::NoContent,
        );
        assert!(
            html.contains("<strong>héll</strong>o"),
            "multibyte boundary respected:\n{html}"
        );
    }
}
