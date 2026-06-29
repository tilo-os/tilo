//! Parse the `word/document.xml` `w:body` into canonical blocks + marks.
//! Styles (`word/styles.xml`) and numbering (`word/numbering.xml`) are pre-parsed
//! into a [`StyleRegistry`]; this walk resolves paragraph/heading/list/character
//! styles against it. Paragraphs become `paragraph`/`heading`/`list_item` blocks
//! depending on `w:pStyle` and `w:numPr`; runs (`w:r`/`w:rPr`/`w:t`) map onto
//! inline marks; tabs/breaks, alignment, fonts and tables all map onto the model.
//! Unmodeled body-level elements are captured verbatim as fidelity atoms.

use super::styles::{attr_val, mark_from_rpr_child, map_jc, StyleRegistry};
use crate::document::MarkValue;
use crate::format::CodecError;
use crate::model::DocMut;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// Open a fresh top-level block anchored after the previous one.
fn start_block(doc: &mut DocMut, prev: &mut Option<String>, cur: &mut Option<String>, cur_len: &mut usize, kind: &str) -> String {
    let id = doc.insert_block(prev.as_deref(), kind);
    *prev = Some(id.clone());
    *cur = Some(id.clone());
    *cur_len = 0;
    id
}

/// Append literal text (tab / break) to the current block.
fn append(doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str) {
    if let Some(b) = cur {
        if !txt.is_empty() {
            doc.splice_text(b, *cur_len, 0, txt);
            *cur_len += txt.chars().count();
        }
    }
}

pub(crate) fn parse_body(document_xml: &[u8], reg: &StyleRegistry, doc: &mut DocMut) -> Result<(), CodecError> {
    let s = std::str::from_utf8(document_xml).map_err(|e| CodecError::Parse(e.to_string()))?;
    let mut reader = Reader::from_str(s);

    let mut in_body = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;

    // Paragraph-property scratch, gathered from `w:pPr` then applied at its close.
    let mut in_ppr = false;
    let mut in_numpr = false;
    let mut p_style: Option<String> = None;
    let mut p_align: Option<String> = None;
    let mut p_numid: Option<String> = None;

    // Run state. Marks are collected for the run's `w:rPr` and applied over the
    // run's text range at `</w:r>` (so later text doesn't expand the mark).
    let mut in_run = false;
    let mut in_rpr = false;
    let mut in_t = false;
    let mut run_start: usize = 0;
    let mut run_marks: Vec<(String, MarkValue)> = Vec::new();

    // (block, start, end, name, value), applied after all text is present.
    let mut pending_marks: Vec<(String, usize, usize, String, MarkValue)> = Vec::new();
    // Paragraph-style inline marks for the current block, applied over [0, len].
    let mut cur_para_marks: Vec<(String, MarkValue)> = Vec::new();

    // Table state: cells become consecutive `cell` blocks tagged table/row/col.
    let mut table_id: Option<String> = None;
    let mut trow: usize = 0;
    let mut tcol: usize = 0;
    let mut table_seq: usize = 0;

    let mut capture: Option<(usize, i32)> = None; // (start byte, open depth) of a fidelity atom

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

    // Finalise the just-parsed paragraph's properties onto its (already opened)
    // block: pick the kind (list_item / heading / paragraph), set attrs, and stash
    // the whole-block paragraph-style marks for plain paragraphs.
    macro_rules! finalize_ppr {
        () => {
            if let Some(id) = cur.clone() {
                let level = p_style.as_deref().and_then(|s| reg.heading_level(s));
                if let Some(numid) = p_numid.clone() {
                    doc.set_kind(&id, "list_item");
                    doc.set_attr(&id, "ordered", reg.num_ordered.get(&numid).copied().unwrap_or(false));
                } else if let Some(l) = level {
                    doc.set_kind(&id, "heading");
                    doc.set_attr(&id, "level", l);
                }
                // Direct `w:jc` wins; otherwise inherit alignment from the style.
                let align = p_align.clone().or_else(|| p_style.as_deref().and_then(|s| reg.para_align(s)));
                if let Some(a) = align {
                    doc.set_attr(&id, "align", a.as_str());
                }
                if level.is_none() && p_numid.is_none() {
                    if let Some(st) = p_style.clone() {
                        cur_para_marks = reg.para_text_marks(&st);
                    }
                }
            }
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
                b"w:body" => in_body = true,
                // A paragraph (outside a table) opens a block; its kind is refined
                // once `w:pPr` closes.
                b"w:p" if in_body && table_id.is_none() => {
                    start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                    p_style = None;
                    p_align = None;
                    p_numid = None;
                    cur_para_marks.clear();
                }
                b"w:pPr" if table_id.is_none() => in_ppr = true,
                b"w:numPr" if in_ppr => in_numpr = true,
                b"w:r" if in_body => {
                    in_run = true;
                    run_start = cur_len;
                    run_marks.clear();
                }
                b"w:rPr" if in_run => in_rpr = true,
                b"w:t" if in_run => in_t = true,
                b"w:tbl" if in_body => {
                    table_seq += 1;
                    table_id = Some(format!("t{table_seq}"));
                    trow = 0;
                }
                b"w:tr" if table_id.is_some() => tcol = 0,
                b"w:tc" if table_id.is_some() => {
                    if let Some(tid) = table_id.clone() {
                        let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "cell");
                        doc.set_attr(&id, "table", tid.as_str());
                        doc.set_attr(&id, "row", trow as i64);
                        doc.set_attr(&id, "col", tcol as i64);
                    }
                }
                // Unknown block-level element → capture verbatim as a fidelity atom.
                _ if in_body && cur.is_none() && table_id.is_none() => {
                    capture = Some((pos_before, 1));
                }
                _ => {}
            },
            Event::Empty(e) => match e.name().as_ref() {
                b"w:p" if in_body && table_id.is_none() => {
                    start_block(doc, &mut prev, &mut cur, &mut cur_len, "paragraph");
                    cur = None;
                }
                b"w:pStyle" if in_ppr => p_style = attr_val(&e, b"w:val"),
                b"w:jc" if in_ppr => p_align = attr_val(&e, b"w:val").map(|v| map_jc(&v)),
                b"w:numId" if in_numpr => p_numid = attr_val(&e, b"w:val"),
                b"w:tab" if in_run => append(doc, &cur, &mut cur_len, "\t"),
                b"w:br" | b"w:cr" if in_run => append(doc, &cur, &mut cur_len, "\n"),
                _ if in_rpr => {
                    if let Some((k, v)) = mark_from_rpr_child(&e) {
                        run_marks.retain(|(k2, _)| k2 != &k);
                        run_marks.push((k, v));
                    } else if e.name().as_ref() == b"w:rStyle" {
                        if let Some(name) = attr_val(&e, b"w:val") {
                            for (k, v) in reg.text_marks(&name) {
                                run_marks.retain(|(k2, _)| k2 != &k);
                                run_marks.push((k, v));
                            }
                        }
                    }
                }
                // Unknown block-level empty element → fidelity atom.
                _ if in_body && cur.is_none() && table_id.is_none() => {
                    let raw = s[pos_before..reader.buffer_position() as usize].to_string();
                    let id = start_block(doc, &mut prev, &mut cur, &mut cur_len, "fidelity");
                    doc.set_attr(&id, "xml", raw.as_str());
                    cur = None;
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"w:body" => in_body = false,
                b"w:pPr" if table_id.is_none() => {
                    in_ppr = false;
                    finalize_ppr!();
                }
                b"w:numPr" => in_numpr = false,
                b"w:rPr" => in_rpr = false,
                b"w:t" => in_t = false,
                b"w:r" => {
                    in_run = false;
                    if let Some(b) = cur.as_ref() {
                        for (name, value) in run_marks.drain(..) {
                            pending_marks.push((b.clone(), run_start, cur_len, name, value));
                        }
                    }
                    run_marks.clear();
                }
                b"w:p" if table_id.is_none() => {
                    flush_para_marks!();
                    cur = None;
                }
                b"w:tc" if table_id.is_some() => {
                    cur = None;
                    tcol += 1;
                }
                b"w:tr" if table_id.is_some() => trow += 1,
                b"w:tbl" => {
                    table_id = None;
                    cur = None;
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_t {
                    if let Some(b) = cur.as_ref() {
                        let txt = t.unescape().map_err(|e| CodecError::Parse(format!("text: {e}")))?;
                        if !txt.is_empty() {
                            doc.splice_text(b, cur_len, 0, &txt);
                            cur_len += txt.chars().count();
                        }
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
    use super::super::container::{minimal_docx, minimal_docx_numbered, minimal_docx_styled};
    use super::super::DocxCodec;
    use crate::{AttrVal, Document, FormatProvider};

    #[test]
    fn imports_headings_and_paragraphs() {
        let body = concat!(
            r#"<w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(2)));
        assert_eq!(snap.blocks[1].kind, "paragraph");
        assert_eq!(snap.blocks[1].text, "Hello");
    }

    #[test]
    fn imports_heading_via_named_style_with_style_alignment() {
        // A real styles.xml: a custom paragraph style derived from Heading1 that
        // also centres its text. Heading detection + alignment both come from the
        // resolved style, not from anything on the paragraph itself.
        let styles = concat!(
            r#"<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/><w:jc w:val="center"/></w:pPr></w:style>"#,
            r#"<w:style w:type="paragraph" w:styleId="MyTitle"><w:basedOn w:val="Heading1"/></w:style>"#,
        );
        let body = r#"<w:p><w:pPr><w:pStyle w:val="MyTitle"/></w:pPr><w:r><w:t>My Title</w:t></w:r></w:p>"#;
        let docx = minimal_docx_styled(styles, body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.kind, "heading", "named paragraph style → heading");
        assert_eq!(b.text, "My Title");
        assert_eq!(b.attrs.get("level"), Some(&AttrVal::Int(1)), "level inherited via basedOn");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())), "alignment from the style");
    }

    #[test]
    fn imports_run_props_as_marks() {
        use crate::document::MarkValue;
        let body = concat!(
            r#"<w:p>"#,
            r#"<w:r><w:t xml:space="preserve">a </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r>"#,
            r#"<w:r><w:t xml:space="preserve"> and </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:i/><w:u w:val="single"/><w:strike/></w:rPr><w:t>more</w:t></w:r>"#,
            r#"</w:p>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let p = &d.materialize().blocks[0];
        assert_eq!(p.text, "a bold and more");
        assert!(p.marks.iter().any(|m| m.name == "strong" && m.start == 2 && m.end == 6), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "em" && m.start == 11 && m.end == 15), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "underline" && m.start == 11 && m.end == 15), "{:?}", p.marks);
        assert!(p.marks.iter().any(|m| m.name == "strike" && m.start == 11 && m.end == 15), "{:?}", p.marks);
        let _ = MarkValue::Bool(true);
    }

    #[test]
    fn imports_align_font_size_and_family() {
        use crate::document::MarkValue;
        let body = concat!(
            r#"<w:p><w:pPr><w:jc w:val="center"/></w:pPr>"#,
            r#"<w:r><w:rPr><w:sz w:val="40"/><w:rFonts w:ascii="Times New Roman"/></w:rPr><w:t>big</w:t></w:r>"#,
            r#"</w:p>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let b = &d.materialize().blocks[0];
        assert_eq!(b.text, "big");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())), "{:?}", b.attrs);
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "{:?}", b.marks);
    }

    #[test]
    fn imports_named_character_style_and_whitespace() {
        let styles = r#"<w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>"#;
        let body = concat!(
            r#"<w:p><w:r><w:t>x</w:t></w:r>"#,
            r#"<w:r><w:rPr><w:rStyle w:val="Strong"/></w:rPr><w:t>y</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>one</w:t><w:br/><w:t>two</w:t><w:tab/><w:t>three</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx_styled(styles, body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let p0 = &snap.blocks[0];
        assert_eq!(p0.text, "xy");
        assert!(p0.marks.iter().any(|m| m.name == "strong" && m.start == 1 && m.end == 2), "named char style → mark: {:?}", p0.marks);
        let p1 = &snap.blocks[1];
        assert!(p1.text.contains('\n'), "w:br → newline: {:?}", p1.text);
        assert!(p1.text.contains('\t'), "w:tab → tab: {:?}", p1.text);
        assert_eq!(p1.text, "one\ntwo\tthree");
    }

    #[test]
    fn imports_numbered_and_bulleted_lists() {
        let numbering = concat!(
            r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>"#,
            r#"<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>"#,
            r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#,
            r#"<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#,
        );
        let body = concat!(
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>first</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>bee</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx_numbered("", numbering, body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 2, "{snap:?}");
        assert_eq!(items[0].text, "first");
        assert_eq!(items[0].attrs.get("ordered"), Some(&AttrVal::Bool(true)), "decimal numId → ordered");
        assert_eq!(items[1].attrs.get("ordered"), Some(&AttrVal::Bool(false)), "bullet numId → unordered");
    }

    #[test]
    fn imports_table_as_editable_cells() {
        let body = concat!(
            r#"<w:tbl>"#,
            r#"<w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr>"#,
            r#"<w:tr><w:tc><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>d</w:t></w:r></w:p></w:tc></w:tr>"#,
            r#"</w:tbl>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.len(), 4, "2x2 cells: {snap:?}");
        assert_eq!(cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"]);
        assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
        assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));
    }

    #[test]
    fn captures_unmodeled_body_element_as_fidelity_atom() {
        let body = concat!(
            r#"<w:p><w:r><w:t>before</w:t></w:r></w:p>"#,
            r#"<w:sdt><w:sdtContent><w:r><w:t>boxed</w:t></w:r></w:sdtContent></w:sdt>"#,
            r#"<w:p><w:r><w:t>after</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx(body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        assert_eq!(snap.blocks.len(), 3, "before / fidelity / after: {snap:?}");
        assert_eq!(snap.blocks[0].text, "before");
        assert_eq!(snap.blocks[1].kind, "fidelity");
        assert_eq!(snap.blocks[2].text, "after");
        let raw = match snap.blocks[1].attrs.get("xml") {
            Some(AttrVal::Str(x)) => x.clone(),
            other => panic!("fidelity atom missing xml attr: {other:?}"),
        };
        assert!(raw.starts_with("<w:sdt") && raw.contains("boxed") && raw.ends_with("</w:sdt>"), "captured raw: {raw}");
    }
}
