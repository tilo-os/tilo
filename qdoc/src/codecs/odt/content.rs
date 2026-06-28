//! Parse the `content.xml` `office:text` body into canonical blocks + marks.
//! Styles (named in `styles.xml`, automatic in `content.xml`) are pre-parsed
//! into a [`StyleRegistry`]; this walk resolves paragraph/heading/list/text
//! styles against it. Headings are recognised from `text:h` *and* from
//! paragraphs using a heading paragraph-style; inline spans, alignment, font,
//! list numbering, tabs/breaks/spaces, and tables all map onto the model.
//! Unmodeled body elements are captured verbatim as fidelity atoms.

use super::styles::{attr_val, StyleRegistry};
use crate::document::MarkValue;
use crate::format::CodecError;
use crate::model::DocMut;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Open a fresh top-level block anchored after the previous one.
fn start_block(doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize, kind: &str) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

fn outline_level(e: &BytesStart) -> Option<i64> {
    attr_val(e, b"text:outline-level").and_then(|v| v.parse::<i64>().ok())
}

/// Append literal text (e.g. tab / line-break / spaces) to the current block.
fn append(doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str) {
    if let Some(b) = cur {
        if !txt.is_empty() {
            doc.splice_text(b, *cur_len, 0, txt);
            *cur_len += txt.chars().count();
        }
    }
}

/// Open a paragraph or heading depending on the referenced paragraph style, set
/// level/alignment, and return the paragraph-level inline marks to apply over the
/// whole block (empty for headings — their boldness comes from the heading kind).
fn open_para(doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize, reg: &StyleRegistry, style: Option<&str>) -> Vec<(String, MarkValue)> {
    let level = style.and_then(|s| reg.heading_level(s));
    let id = start_block(doc, prev, cur, cur_len, if level.is_some() { "heading" } else { "paragraph" });
    if let Some(l) = level {
        doc.set_attr(&id, "level", l);
    }
    if let Some(a) = style.and_then(|s| reg.para_align(s)) {
        doc.set_attr(&id, "align", a.as_str());
    }
    if level.is_some() {
        Vec::new()
    } else {
        style.map(|s| reg.para_text_marks(s)).unwrap_or_default()
    }
}

pub(crate) fn parse_body(content_xml: &[u8], reg: &StyleRegistry, doc: &mut DocMut) -> Result<(), CodecError> {
    let s = std::str::from_utf8(content_xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);

    let mut in_text = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_item = false;

    // Span marks + paragraph-style marks, applied after all text is in (so text
    // appended after a span doesn't expand its mark).
    let mut span_stack: Vec<(Vec<(String, MarkValue)>, usize)> = Vec::new();
    // Open `text:a` hyperlinks: (href, style-marks from the link's own style, start).
    let mut link_stack: Vec<(Option<String>, Vec<(String, MarkValue)>, usize)> = Vec::new();
    let mut pending_marks: Vec<(String, usize, usize, String, MarkValue)> = Vec::new();
    // Paragraph-style inline marks for the current block, applied over [0, len].
    let mut cur_para_marks: Vec<(String, MarkValue)> = Vec::new();
    // Nested list ordered-ness, from each list's list-style.
    let mut list_stack: Vec<bool> = Vec::new();

    // Table state: cells become consecutive `cell` blocks tagged table/row/col.
    let mut table_id: Option<String> = None;
    let mut trow: usize = 0;
    let mut tcol: usize = 0;
    let mut table_seq: usize = 0;

    let mut capture: Option<(usize, i32)> = None; // (start byte, open depth) of a fidelity atom

    // Flush the current block's paragraph-style marks over its full text range.
    macro_rules! flush_para_marks {
        () => {
            if let Some(b) = cur.as_ref() {
                for (name, value) in cur_para_marks.drain(..) {
                    pending_marks.push((b.clone(), 0, cur_len, name, value));
                }
            }
            cur_para_marks.clear();
        };
    }

    loop {
        let pos_before = reader.buffer_position() as usize;
        let ev = reader.read_event().map_err(|e| CodecError::Parse(format!("xml: {e}")))?;

        if let Some((start, depth)) = capture.as_mut() {
            match &ev {
                Event::Start(_) => *depth += 1,
                Event::End(_) => {
                    *depth -= 1;
                    if *depth == 0 {
                        let raw = s[*start..reader.buffer_position() as usize].to_string();
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                        doc.set_attr(&id, "xml", raw.as_str());
                        cur = None;
                        capture = None;
                    }
                }
                _ => {}
            }
            continue;
        }

        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"office:text" => in_text = true,
                b"text:h" if in_text => {
                    let style = attr_val(&e, b"text:style-name");
                    let lvl = outline_level(&e).or_else(|| style.as_deref().and_then(|s| reg.heading_level(s))).unwrap_or(1).clamp(1, 6);
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                    doc.set_attr(&id, "level", lvl);
                    if let Some(a) = style.as_deref().and_then(|s| reg.para_align(s)) {
                        doc.set_attr(&id, "align", a.as_str());
                    }
                    cur_para_marks.clear();
                }
                b"text:p" if in_text && !in_item && table_id.is_none() => {
                    let style = attr_val(&e, b"text:style-name");
                    cur_para_marks = open_para(doc, &mut prev, &mut cur, &mut cur_len, reg, style.as_deref());
                }
                b"text:list" if in_text => {
                    let ordered = attr_val(&e, b"text:style-name").and_then(|s| reg.list_ordered.get(&s).copied()).unwrap_or(false);
                    list_stack.push(ordered);
                }
                b"text:list-item" if in_text => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "list_item");
                    doc.set_attr(&id, "ordered", *list_stack.last().unwrap_or(&false));
                    in_item = true;
                }
                b"text:p" if in_text && in_item => {
                    if let Some(id) = cur.clone() {
                        if let Some(a) = attr_val(&e, b"text:style-name").as_deref().and_then(|s| reg.para_align(s)) {
                            doc.set_attr(&id, "align", a.as_str());
                        }
                    }
                }
                b"text:p" if in_text && table_id.is_some() => {
                    // The paragraph inside a table cell: lift its alignment onto the
                    // cell block so centred/right-aligned cells round-trip.
                    if let Some(id) = cur.clone() {
                        if let Some(a) = attr_val(&e, b"text:style-name").as_deref().and_then(|s| reg.para_align(s)) {
                            doc.set_attr(&id, "align", a.as_str());
                        }
                    }
                }
                b"table:table" if in_text => {
                    table_id = Some(attr_val(&e, b"table:name").unwrap_or_else(|| {
                        table_seq += 1;
                        format!("t{table_seq}")
                    }));
                    trow = 0;
                }
                b"table:table-row" if in_text => tcol = 0,
                b"table:table-cell" if in_text => {
                    if let Some(tid) = table_id.clone() {
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "cell");
                        doc.set_attr(&id, "table", tid.as_str());
                        doc.set_attr(&id, "row", trow as i64);
                        doc.set_attr(&id, "col", tcol as i64);
                    }
                }
                b"text:span" if in_text => {
                    let marks = attr_val(&e, b"text:style-name").map(|s| reg.text_marks(&s)).unwrap_or_default();
                    span_stack.push((marks, cur_len));
                }
                b"text:a" if in_text => {
                    // A hyperlink: model the href as an inline `link` mark over the
                    // linked range (so it merges with concurrent text edits), plus
                    // any text marks the link's own style carries (underline/colour).
                    let href = attr_val(&e, b"xlink:href");
                    let marks = attr_val(&e, b"text:style-name").map(|s| reg.text_marks(&s)).unwrap_or_default();
                    link_stack.push((href, marks, cur_len));
                }
                _ if in_text && cur.is_none() && !in_item && table_id.is_none() => {
                    capture = Some((pos_before, 1));
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"text:p" if in_text && !in_item && table_id.is_none() => {
                    open_para(doc, &mut prev, &mut cur, &mut cur_len, reg, attr_val(&e, b"text:style-name").as_deref());
                    cur = None;
                }
                b"text:h" if in_text => {
                    let style = attr_val(&e, b"text:style-name");
                    let lvl = outline_level(&e).or_else(|| style.as_deref().and_then(|s| reg.heading_level(s))).unwrap_or(1).clamp(1, 6);
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                    doc.set_attr(&id, "level", lvl);
                    cur = None;
                }
                b"text:tab" if in_text => append(doc, &cur, &mut cur_len, "\t"),
                b"text:line-break" if in_text => append(doc, &cur, &mut cur_len, "\n"),
                b"text:s" if in_text => {
                    let n = attr_val(&e, b"text:c").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).min(256);
                    append(doc, &cur, &mut cur_len, &" ".repeat(n));
                }
                b"table:table-cell" if in_text => {
                    if let Some(tid) = table_id.clone() {
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "cell");
                        doc.set_attr(&id, "table", tid.as_str());
                        doc.set_attr(&id, "row", trow as i64);
                        doc.set_attr(&id, "col", tcol as i64);
                        cur = None;
                    }
                    tcol += 1;
                }
                _ if in_text && cur.is_none() && !in_item && table_id.is_none() => {
                    let raw = s[pos_before..reader.buffer_position() as usize].to_string();
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                    doc.set_attr(&id, "xml", raw.as_str());
                    cur = None;
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"office:text" => in_text = false,
                b"text:h" => {
                    cur = None;
                }
                b"text:p" if !in_item && table_id.is_none() => {
                    flush_para_marks!();
                    cur = None;
                }
                b"text:list" => {
                    list_stack.pop();
                }
                b"table:table" => {
                    table_id = None;
                    cur = None;
                }
                b"table:table-row" => trow += 1,
                b"table:table-cell" => {
                    cur = None;
                    tcol += 1;
                }
                b"text:list-item" => {
                    flush_para_marks!();
                    cur = None;
                    in_item = false;
                }
                b"text:span" if in_text => {
                    if let Some((marks, start)) = span_stack.pop() {
                        if let Some(b) = cur.as_ref() {
                            for (name, value) in marks {
                                pending_marks.push((b.clone(), start, cur_len, name, value));
                            }
                        }
                    }
                }
                b"text:a" if in_text => {
                    if let Some((href, marks, start)) = link_stack.pop() {
                        if let Some(b) = cur.as_ref() {
                            if let Some(h) = href {
                                pending_marks.push((b.clone(), start, cur_len, "link".to_string(), MarkValue::Str(h)));
                            }
                            for (name, value) in marks {
                                pending_marks.push((b.clone(), start, cur_len, name, value));
                            }
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(b) = cur.as_ref() {
                    let txt = t.unescape().map_err(|e| CodecError::Parse(format!("text: {e}")))?;
                    if !txt.is_empty() {
                        doc.splice_text(b, cur_len, 0, &txt);
                        cur_len += txt.chars().count();
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    // Apply marks now that all text is present (exact ranges, no expansion).
    for (b, start, end, name, value) in pending_marks {
        if start >= end {
            continue;
        }
        match value {
            MarkValue::Bool(v) => doc.add_mark(&b, start, end, &name, v),
            MarkValue::Int(n) => doc.add_mark(&b, start, end, &name, n),
            MarkValue::Str(s) => doc.add_mark(&b, start, end, &name, s.as_str()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::container::{minimal_odt, minimal_odt_styled};
    use super::super::OdtCodec;
    use crate::{AttrVal, Document, FormatProvider};

    #[test]
    fn imports_headings_paragraphs_lists() {
        let body = concat!(
            r#"<text:h text:outline-level="2">Title</text:h>"#,
            r#"<text:p>Hello</text:p>"#,
            r#"<text:list><text:list-item><text:p>one</text:p></text:list-item>"#,
            r#"<text:list-item><text:p>two</text:p></text:list-item></text:list>"#,
        );
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(2)));
        assert_eq!(snap.blocks[1].kind, "paragraph");
        assert_eq!(snap.blocks[1].text, "Hello");
        assert_eq!(snap.blocks.iter().filter(|b| b.kind == "list_item").count(), 2);
        assert_eq!(snap.blocks[2].text, "one");
        assert_eq!(snap.blocks[3].text, "two");
    }

    #[test]
    fn imports_spans_as_marks() {
        let styles = concat!(
            r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#,
            r#"<style:style style:name="T2" style:family="text"><style:text-properties fo:font-style="italic"/></style:style>"#,
        );
        let body = concat!(
            r#"<text:p>a <text:span text:style-name="T1">bold</text:span>"#,
            r#" and <text:span text:style-name="T2">em</text:span></text:p>"#,
        );
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let p = &snap.blocks[0];
        assert_eq!(p.text, "a bold and em");
        assert!(
            p.marks.iter().any(|m| m.name == "strong" && m.start == 2 && m.end == 6),
            "strong over 'bold', got {:?}",
            p.marks
        );
        assert!(
            p.marks.iter().any(|m| m.name == "em" && m.start == 11 && m.end == 13),
            "em over 'em', got {:?}",
            p.marks
        );
    }

    #[test]
    fn imports_paragraph_align_and_inline_font() {
        use crate::document::MarkValue;
        // Alignment comes from the paragraph style; font from a text span.
        let styles = concat!(
            r#"<style:style style:name="P2" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>"#,
            r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-size="20pt" style:font-name="Liberation Serif"/></style:style>"#,
        );
        let body = r#"<text:p text:style-name="P2"><text:span text:style-name="T1">big</text:span></text:p>"#;
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "big");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())), "{:?}", b.attrs);
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "{:?}", b.marks);
    }

    #[test]
    fn imports_named_heading_style_whitespace_and_ordered_list() {
        // The kind of structure LibreOffice emits: headings via named paragraph
        // styles, numbered lists via a list-style, explicit breaks/tabs.
        let styles = concat!(
            r#"<style:style style:name="Heading_20_1" style:display-name="Heading 1" style:family="paragraph" style:default-outline-level="1"/>"#,
            r#"<style:style style:name="MySub" style:family="paragraph" style:parent-style-name="Heading_20_1"/>"#,
            r#"<text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style>"#,
        );
        let body = concat!(
            r#"<text:p text:style-name="Heading_20_1">My Title</text:p>"#,
            r#"<text:p text:style-name="MySub">A Subtitle</text:p>"#,
            r#"<text:p>Line one<text:line-break/>line two<text:tab/>after a tab and<text:s text:c="3"/>spaces.</text:p>"#,
            r#"<text:list text:style-name="L1"><text:list-item><text:p>first</text:p></text:list-item><text:list-item><text:p>second</text:p></text:list-item></text:list>"#,
        );
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(snap.blocks[0].kind, "heading", "named paragraph style → heading: {snap:?}");
        assert_eq!(snap.blocks[0].text, "My Title");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(1)));
        assert_eq!(snap.blocks[1].kind, "heading", "heading via inherited parent style");
        assert!(snap.blocks[2].text.contains('\n'), "line-break: {:?}", snap.blocks[2].text);
        assert!(snap.blocks[2].text.contains('\t'), "tab: {:?}", snap.blocks[2].text);
        assert!(snap.blocks[2].text.contains("and   spaces"), "text:s spaces: {:?}", snap.blocks[2].text);
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].attrs.get("ordered"), Some(&AttrVal::Bool(true)), "numbered list-style → ordered");
    }

    #[test]
    fn table_roundtrips_as_editable_cells() {
        let body = concat!(
            r#"<table:table table:name="T1">"#,
            r#"<table:table-row><table:table-cell><text:p>a</text:p></table:table-cell>"#,
            r#"<table:table-cell><text:p>b</text:p></table:table-cell></table:table-row>"#,
            r#"<table:table-row><table:table-cell><text:p>c</text:p></table:table-cell>"#,
            r#"<table:table-cell><text:p>d</text:p></table:table-cell></table:table-row>"#,
            r#"</table:table>"#,
        );
        let odt = minimal_odt_styled("", body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 4, "2x2 cells: {snap:?}");
        assert_eq!(cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"]);
        assert_eq!(cells[0].attrs.get("table"), Some(&AttrVal::Str("T1".into())));
        assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
        assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));

        // Round-trip: export then re-import → still a 2x2 table with the same text.
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let cells2: Vec<_> = d2.materialize().blocks.iter().filter(|b| b.kind == "cell").map(|b| b.text.clone()).collect();
        assert_eq!(cells2, vec!["a", "b", "c", "d"], "table survives export+import");
    }

    #[test]
    fn imports_hyperlink_as_link_mark() {
        use crate::document::MarkValue;
        // A `text:a` becomes an inline `link` mark carrying the href, so it merges
        // with concurrent text edits instead of being atomized.
        let body = r#"<text:p>see <text:a xlink:href="https://qos.dev/x">here</text:a>!</text:p>"#;
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "see here!");
        let link = b.marks.iter().find(|m| m.name == "link").expect("link mark present");
        assert_eq!((link.start, link.end), (4, 8), "link over 'here': {:?}", b.marks);
        assert_eq!(link.value, MarkValue::Str("https://qos.dev/x".into()));
    }

    #[test]
    fn imports_link_wrapping_styled_span() {
        use crate::document::MarkValue;
        // A link whose inner text is also bold: link mark + strong both survive.
        let styles = r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#;
        let body = r#"<text:p><text:a xlink:href="http://x/"><text:span text:style-name="T1">bold</text:span></text:a></text:p>"#;
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "bold");
        assert!(b.marks.iter().any(|m| m.name == "link" && m.value == MarkValue::Str("http://x/".into())), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "strong"), "{:?}", b.marks);
    }

    #[test]
    fn imports_text_color_as_mark() {
        use crate::document::MarkValue;
        let styles = r##"<style:style style:name="C1" style:family="text"><style:text-properties fo:color="#FF0000"/></style:style>"##;
        let body = r#"<text:p>a <text:span text:style-name="C1">red</text:span></text:p>"#;
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "a red");
        assert!(
            b.marks.iter().any(|m| m.name == "color" && m.start == 2 && m.end == 5 && m.value == MarkValue::Str("#ff0000".into())),
            "color mark normalized + ranged: {:?}",
            b.marks
        );
    }

    #[test]
    fn cell_alignment_roundtrips() {
        // A centred table cell: alignment lives on the inner paragraph in ODF.
        let styles = r#"<style:style style:name="Pc" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>"#;
        let body = concat!(
            r#"<table:table table:name="T1"><table:table-row>"#,
            r#"<table:table-cell><text:p text:style-name="Pc">mid</text:p></table:table-cell>"#,
            r#"<table:table-cell><text:p>plain</text:p></table:table-cell>"#,
            r#"</table:table-row></table:table>"#,
        );
        let odt = minimal_odt_styled(styles, body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let cells: Vec<_> = d.materialize().blocks.into_iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].attrs.get("align"), Some(&AttrVal::Str("center".into())), "centred cell: {:?}", cells[0].attrs);
        assert_eq!(cells[1].attrs.get("align"), None, "plain cell has no align");

        // Round-trip: export then re-import → the centred cell stays centred.
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let aligns: Vec<_> = d2.materialize().blocks.iter().filter(|b| b.kind == "cell").map(|b| b.attrs.get("align").cloned()).collect();
        assert_eq!(aligns, vec![Some(AttrVal::Str("center".into())), None], "cell alignment round-trips");
    }

    #[test]
    fn preserves_unmodeled_body_element_as_fidelity_atom() {
        let body = concat!(
            r#"<text:p>before</text:p>"#,
            r#"<draw:frame draw:name="F"><draw:image xlink:href="pic.png"/></draw:frame>"#,
            r#"<text:p>after</text:p>"#,
        );
        let odt = minimal_odt_styled("", body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(snap.blocks.len(), 3, "before / fidelity / after: {snap:?}");
        assert_eq!(snap.blocks[0].kind, "paragraph");
        assert_eq!(snap.blocks[0].text, "before");
        assert_eq!(snap.blocks[1].kind, "fidelity");
        assert_eq!(snap.blocks[2].text, "after");
        let raw1 = match snap.blocks[1].attrs.get("xml") {
            Some(AttrVal::Str(x)) => x.clone(),
            other => panic!("fidelity atom missing xml attr: {other:?}"),
        };
        assert!(
            raw1.starts_with("<draw:frame") && raw1.contains("pic.png") && raw1.ends_with("</draw:frame>"),
            "captured raw: {raw1}"
        );

        // Round-trip: export then re-import → the table atom survives verbatim.
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap2 = d2.materialize();
        assert_eq!(snap2.blocks.len(), 3);
        let raw2 = match snap2.blocks[1].attrs.get("xml") {
            Some(AttrVal::Str(x)) => x.clone(),
            other => panic!("fidelity atom missing after round-trip: {other:?}"),
        };
        assert_eq!(raw1, raw2, "fidelity atom round-trips verbatim");
    }
}
