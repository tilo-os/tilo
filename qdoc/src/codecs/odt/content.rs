//! Parse the `content.xml` `office:text` body into canonical blocks + marks.
//! Block structure (Task 2): headings / paragraphs / list items + text.
//! Inline formatting (Task 3): `office:automatic-styles` is pre-scanned to map
//! each text style-name to a set of marks, and `text:span` references apply them
//! over the span's text range. Unknown spans keep their text (preservation of
//! the span element itself is Task 5).

use crate::document::MarkValue;
use crate::format::CodecError;
use crate::model::DocMut;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::HashMap;

/// Open a fresh top-level block anchored after the previous one.
fn start_block(
    doc: &mut DocMut,
    prev: &mut Option<String>,
    cur: &mut Option<String>,
    cur_len: &mut usize,
    kind: &str,
) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn outline_level(e: &BytesStart) -> Option<i64> {
    attr_val(e, b"text:outline-level").and_then(|v| v.parse::<i64>().ok())
}

/// Marks (with values) implied by a `style:text-properties` element. Font size
/// and family become valued inline marks (`fontSize`/`fontFamily`).
fn marks_from_text_props(e: &BytesStart) -> Vec<(String, MarkValue)> {
    let mut m = Vec::new();
    for a in e.attributes().flatten() {
        let val = a.unescape_value().unwrap_or_default();
        match a.key.as_ref() {
            b"fo:font-weight" if val == "bold" => m.push(("strong".into(), MarkValue::Bool(true))),
            b"fo:font-style" if val == "italic" => m.push(("em".into(), MarkValue::Bool(true))),
            b"style:text-underline-style" if val != "none" => m.push(("underline".into(), MarkValue::Bool(true))),
            b"fo:font-size" => {
                if let Some(n) = parse_pt(&val) {
                    m.push(("fontSize".into(), MarkValue::Int(n)));
                }
            }
            b"style:font-name" => m.push(("fontFamily".into(), MarkValue::Str(generic_font(&val)))),
            _ => {}
        }
    }
    m
}

/// Record the marks for the text style currently being defined.
fn record_style(e: &BytesStart, cur_style: &Option<String>, styles: &mut HashMap<String, Vec<(String, MarkValue)>>) {
    if let Some(name) = cur_style {
        let marks = marks_from_text_props(e);
        if !marks.is_empty() {
            styles.insert(name.clone(), marks);
        }
    }
}

fn parse_pt(v: &str) -> Option<i64> {
    v.trim_end_matches("pt").trim().parse::<f64>().ok().map(|f| f.round() as i64)
}

fn generic_font(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    if n.contains("serif") && !n.contains("sans") {
        "serif".into()
    } else if n.contains("mono") {
        "mono".into()
    } else {
        "sans".into()
    }
}

fn map_align(v: &str) -> String {
    match v {
        "center" => "center".into(),
        "end" | "right" => "right".into(),
        _ => "left".into(),
    }
}

/// Paragraph alignment recovered from a paragraph automatic style. (Font is a
/// per-run inline mark, handled via `style:text-properties` on text spans.)
#[derive(Default, Clone)]
struct ParaProps {
    align: Option<String>,
}

/// Record alignment from a paragraph style's `paragraph-properties`.
fn record_para_align(e: &BytesStart, cur: &Option<String>, fam: &Option<String>, m: &mut HashMap<String, ParaProps>) {
    if fam.as_deref() != Some("paragraph") {
        return;
    }
    if let (Some(name), Some(al)) = (cur, attr_val(e, b"fo:text-align")) {
        m.entry(name.clone()).or_default().align = Some(map_align(&al));
    }
}

/// Apply a referenced paragraph style's alignment to a block.
fn apply_para_style(doc: &mut DocMut, id: &str, e: &BytesStart, m: &HashMap<String, ParaProps>) {
    if let Some(p) = attr_val(e, b"text:style-name").and_then(|s| m.get(&s)) {
        if let Some(a) = &p.align {
            doc.set_attr(id, "align", a.as_str());
        }
    }
}

pub(crate) fn parse_body(content_xml: &[u8], doc: &mut DocMut) -> Result<(), CodecError> {
    let s = std::str::from_utf8(content_xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);

    // Block state.
    let mut in_text = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    let mut in_item = false;

    // Style state.
    let mut in_styles = false;
    let mut cur_style: Option<String> = None; // style being defined
    let mut cur_family: Option<String> = None; // its style:family
    let mut styles: HashMap<String, Vec<(String, MarkValue)>> = HashMap::new();
    let mut para_styles: HashMap<String, ParaProps> = HashMap::new();
    let mut span_stack: Vec<(Vec<(String, MarkValue)>, usize)> = Vec::new();
    // Marks are recorded during the walk and applied *after* all text is in, so
    // text appended after a span doesn't expand its mark (add_mark expands at the
    // trailing boundary).
    let mut pending_marks: Vec<(String, usize, usize, String, MarkValue)> = Vec::new();

    // Table state: cells become consecutive `cell` blocks tagged table/row/col.
    let mut table_id: Option<String> = None;
    let mut trow: usize = 0;
    let mut tcol: usize = 0;
    let mut table_seq: usize = 0;

    // Capture state for unmodeled top-level body elements → fidelity atoms.
    let mut capture: Option<(usize, i32)> = None; // (start byte, open depth)

    loop {
        let pos_before = reader.buffer_position() as usize;
        let ev = reader.read_event().map_err(|e| CodecError::Parse(format!("xml: {e}")))?;

        // While capturing an unmodeled subtree, only track depth; when it closes,
        // stash the raw XML verbatim as a fidelity-atom block.
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
                b"office:automatic-styles" => in_styles = true,
                b"style:style" if in_styles => {
                    cur_family = attr_val(&e, b"style:family");
                    cur_style = attr_val(&e, b"style:name");
                }
                b"style:text-properties" if in_styles => record_style(&e, &cur_style, &mut styles),
                b"style:paragraph-properties" if in_styles => {
                    record_para_align(&e, &cur_style, &cur_family, &mut para_styles);
                }
                b"office:text" => in_text = true,
                b"text:h" if in_text => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                    doc.set_attr(&id, "level", outline_level(&e).unwrap_or(1));
                    apply_para_style(doc, &id, &e, &para_styles);
                }
                b"text:p" if in_text && !in_item && table_id.is_none() => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                    apply_para_style(doc, &id, &e, &para_styles);
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
                b"text:list-item" if in_text => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "list_item");
                    doc.set_attr(&id, "ordered", false);
                    in_item = true;
                }
                b"text:p" if in_text && in_item => {
                    // List-item paragraph carries the item's paragraph style.
                    if let Some(id) = cur.clone() {
                        apply_para_style(doc, &id, &e, &para_styles);
                    }
                }
                b"text:span" if in_text => {
                    let marks = attr_val(&e, b"text:style-name")
                        .and_then(|s| styles.get(&s).cloned())
                        .unwrap_or_default();
                    span_stack.push((marks, cur_len));
                }
                b"text:list" => {} // container; handled via its list items
                // Any other element directly under office:text is unmodeled →
                // capture its whole subtree verbatim as a fidelity atom.
                _ if in_text && cur.is_none() && !in_item && table_id.is_none() => {
                    capture = Some((pos_before, 1));
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"style:text-properties" if in_styles => record_style(&e, &cur_style, &mut styles),
                b"style:paragraph-properties" if in_styles => {
                    record_para_align(&e, &cur_style, &cur_family, &mut para_styles);
                }
                b"text:p" if in_text && !in_item && table_id.is_none() => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                    apply_para_style(doc, &id, &e, &para_styles);
                    cur = None;
                }
                b"text:h" if in_text => {
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "heading");
                    doc.set_attr(&id, "level", outline_level(&e).unwrap_or(1));
                    apply_para_style(doc, &id, &e, &para_styles);
                    cur = None;
                }
                b"table:table-cell" if in_text => {
                    // Self-closing (empty) cell.
                    if let Some(tid) = table_id.clone() {
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "cell");
                        doc.set_attr(&id, "table", tid.as_str());
                        doc.set_attr(&id, "row", trow as i64);
                        doc.set_attr(&id, "col", tcol as i64);
                        cur = None;
                    }
                    tcol += 1;
                }
                // Self-closing unmodeled element under office:text → fidelity atom.
                _ if in_text && cur.is_none() && !in_item && table_id.is_none() => {
                    let raw = s[pos_before..reader.buffer_position() as usize].to_string();
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                    doc.set_attr(&id, "xml", raw.as_str());
                    cur = None;
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"office:automatic-styles" => in_styles = false,
                b"style:style" => {
                    cur_style = None;
                    cur_family = None;
                }
                b"office:text" => in_text = false,
                b"text:h" => cur = None,
                b"text:p" if !in_item && table_id.is_none() => cur = None,
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
