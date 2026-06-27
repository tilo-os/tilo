//! Export the canonical model back to a `.odt`. Phase 4 Task 4 generates a
//! fresh minimal ODF from the model (model round-trip). Verbatim preservation of
//! the *original* container's parts + prologue is Task 5 — this exporter is what
//! Task 5 will splice the generated body into.

use super::container::OdtContainer;
use crate::document::MarkSpan;
use crate::format::CodecError;
use crate::model::DocRef;
use crate::snapshot::{AttrVal, DocSnapshot};
use std::collections::BTreeSet;

const MIMETYPE: &[u8] = b"application/vnd.oasis.opendocument.text";
const MANIFEST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
<manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>
<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;

pub(crate) fn export(doc: DocRef) -> Result<Vec<u8>, CodecError> {
    let snap = doc.materialize();
    let content = build_content_xml(&snap);

    let mut c = OdtContainer { parts: Vec::new() };
    // Re-emit every preserved part verbatim (styles.xml, settings.xml, images,
    // the original manifest/mimetype, …).
    let mut had_mimetype = false;
    let mut had_manifest = false;
    for (key, bytes) in crate::model::preserved_parts(doc.doc, doc.c) {
        if let Some(name) = key.strip_prefix("odt/part/") {
            had_mimetype |= name == "mimetype";
            had_manifest |= name == "META-INF/manifest.xml";
            c.set(name, bytes);
        }
    }
    // Fall back to generated mimetype/manifest when the source had none (e.g. a
    // doc created from scratch rather than imported).
    if !had_mimetype {
        c.set("mimetype", MIMETYPE.to_vec());
    }
    if !had_manifest {
        c.set("META-INF/manifest.xml", MANIFEST.to_vec());
    }
    c.set("content.xml", content.into_bytes());
    c.write()
}

/// Codec-authored automatic style name for a mark.
fn style_for(name: &str) -> Option<&'static str> {
    match name {
        "strong" => Some("qdoc-strong"),
        "em" => Some("qdoc-em"),
        "underline" => Some("qdoc-underline"),
        _ => None,
    }
}

/// The static mark name, for known marks only.
fn static_mark(name: &str) -> Option<&'static str> {
    match name {
        "strong" => Some("strong"),
        "em" => Some("em"),
        "underline" => Some("underline"),
        _ => None,
    }
}

fn style_props(name: &str) -> &'static str {
    match name {
        "strong" => r#"fo:font-weight="bold""#,
        "em" => r#"fo:font-style="italic""#,
        "underline" => r#"style:text-underline-style="solid""#,
        _ => "",
    }
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

/// Render a block's text with `text:span` wrappers for its marks. Spans are
/// closed and re-opened whenever the active mark set changes, so the output is
/// always well-nested even for overlapping marks.
fn render_inline(text: &str, marks: &[MarkSpan], used: &mut BTreeSet<&'static str>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut open: Vec<&'static str> = Vec::new();
    for i in 0..n {
        let mut want: Vec<&'static str> = marks
            .iter()
            .filter(|m| i >= m.start && i < m.end)
            .filter_map(|m| static_mark(&m.name))
            .collect();
        want.sort();
        want.dedup();
        if want != open {
            for _ in 0..open.len() {
                out.push_str("</text:span>");
            }
            open.clear();
            for &name in &want {
                out.push_str(&format!(r#"<text:span text:style-name="{}">"#, style_for(name).unwrap()));
                used.insert(name);
                open.push(name);
            }
        }
        push_escaped(&mut out, chars[i]);
    }
    for _ in 0..open.len() {
        out.push_str("</text:span>");
    }
    out
}

/// ODF font name for a generic family (round-trips with `font_from_name`).
fn font_name(generic: &str) -> &'static str {
    match generic {
        "serif" => "Liberation Serif",
        "mono" => "Liberation Mono",
        _ => "Liberation Sans",
    }
}

/// Paragraph-properties + text-properties XML for a block's align/font/size, if
/// any are non-default.
fn para_props(b: &BlockView) -> Option<(String, String)> {
    let mut pp = String::new();
    let mut tp = String::new();
    if let Some(AttrVal::Str(a)) = b.attrs.get("align") {
        let v = match a.as_str() {
            "center" => "center",
            "right" => "end",
            _ => "",
        };
        if !v.is_empty() {
            pp.push_str(&format!(r#" fo:text-align="{v}""#));
        }
    }
    if let Some(AttrVal::Int(n)) = b.attrs.get("fontSize") {
        tp.push_str(&format!(r#" fo:font-size="{n}pt""#));
    }
    if let Some(AttrVal::Str(f)) = b.attrs.get("font") {
        tp.push_str(&format!(r#" style:font-name="{}""#, font_name(f)));
    }
    if pp.is_empty() && tp.is_empty() {
        None
    } else {
        Some((pp, tp))
    }
}

fn snap_cell_table(b: &BlockView) -> Option<String> {
    match b.attrs.get("table") {
        Some(AttrVal::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn snap_cell_rc(b: &BlockView) -> (usize, usize) {
    let g = |k: &str| match b.attrs.get(k) {
        Some(AttrVal::Int(n)) if *n >= 0 => *n as usize,
        _ => 0,
    };
    (g("row"), g("col"))
}

/// Get-or-create a paragraph automatic style name for these props.
fn register_para_style(styles: &mut Vec<(String, String)>, props: (String, String)) -> String {
    if let Some(i) = styles.iter().position(|p| p == &props) {
        format!("P{}", i + 1)
    } else {
        styles.push(props);
        format!("P{}", styles.len())
    }
}

use crate::snapshot::BlockView;

fn build_content_xml(snap: &DocSnapshot) -> String {
    let mut body = String::new();
    let mut used: BTreeSet<&'static str> = BTreeSet::new();
    let mut pstyles: Vec<(String, String)> = Vec::new();
    let mut style_attr = |b: &BlockView| -> String {
        match para_props(b) {
            Some(props) => format!(r#" text:style-name="{}""#, register_para_style(&mut pstyles, props)),
            None => String::new(),
        }
    };
    let mut i = 0;
    while i < snap.blocks.len() {
        let b = &snap.blocks[i];
        match b.kind.as_str() {
            "heading" => {
                let level = match b.attrs.get("level") {
                    Some(AttrVal::Int(n)) => (*n).clamp(1, 6),
                    _ => 1,
                };
                let sa = style_attr(b);
                body.push_str(&format!(
                    r#"<text:h text:outline-level="{level}"{sa}>{}</text:h>"#,
                    render_inline(&b.text, &b.marks, &mut used)
                ));
                i += 1;
            }
            "list_item" => {
                body.push_str("<text:list>");
                while i < snap.blocks.len() && snap.blocks[i].kind == "list_item" {
                    let li = &snap.blocks[i];
                    let sa = style_attr(li);
                    body.push_str(&format!(
                        r#"<text:list-item><text:p{sa}>{}</text:p></text:list-item>"#,
                        render_inline(&li.text, &li.marks, &mut used)
                    ));
                    i += 1;
                }
                body.push_str("</text:list>");
            }
            "fidelity" => {
                // Re-emit the preserved unmodeled element verbatim, in place.
                if let Some(AttrVal::Str(xml)) = b.attrs.get("xml") {
                    body.push_str(xml);
                }
                i += 1;
            }
            "cell" => {
                // Emit a run of same-`table` cells as a real ODF table.
                let tid = snap_cell_table(b).unwrap_or_default();
                let start = i;
                while i < snap.blocks.len() && snap_cell_table(&snap.blocks[i]).as_deref() == Some(tid.as_str()) {
                    i += 1;
                }
                let cells = &snap.blocks[start..i];
                let nrows = cells.iter().map(|c| snap_cell_rc(c).0).max().unwrap_or(0) + 1;
                let ncols = cells.iter().map(|c| snap_cell_rc(c).1).max().unwrap_or(0) + 1;
                body.push_str(&format!(r#"<table:table table:name="{tid}">"#));
                body.push_str(&format!(r#"<table:table-column table:number-columns-repeated="{ncols}"/>"#));
                for r in 0..nrows {
                    body.push_str("<table:table-row>");
                    for c in 0..ncols {
                        let inner = cells
                            .iter()
                            .find(|cl| snap_cell_rc(cl) == (r, c))
                            .map(|cl| render_inline(&cl.text, &cl.marks, &mut used))
                            .unwrap_or_default();
                        body.push_str(&format!(r#"<table:table-cell><text:p>{inner}</text:p></table:table-cell>"#));
                    }
                    body.push_str("</table:table-row>");
                }
                body.push_str("</table:table>");
            }
            _ => {
                let sa = style_attr(b);
                body.push_str(&format!(
                    r#"<text:p{sa}>{}</text:p>"#,
                    render_inline(&b.text, &b.marks, &mut used)
                ));
                i += 1;
            }
        }
    }

    let mut styles = String::new();
    for &name in &used {
        styles.push_str(&format!(
            r#"<style:style style:name="{}" style:family="text"><style:text-properties {}/></style:style>"#,
            style_for(name).unwrap(),
            style_props(name)
        ));
    }
    for (idx, (pp, tp)) in pstyles.iter().enumerate() {
        styles.push_str(&format!(
            r#"<style:style style:name="P{}" style:family="paragraph"><style:paragraph-properties{pp}/><style:text-properties{tp}/></style:style>"#,
            idx + 1
        ));
    }

    // Declare the common ODF namespaces so re-emitted fidelity atoms (tables,
    // drawings, …) stay well-formed. Truly exotic namespaces are only fully
    // preserved once the original content prologue is carried verbatim (future).
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0">
<office:automatic-styles>{styles}</office:automatic-styles>
<office:body><office:text>{body}</office:text></office:body>
</office:document-content>"#
    )
}

#[cfg(test)]
mod tests {
    use super::super::container::{minimal_odt_styled, OdtContainer};
    use super::super::OdtCodec;
    use crate::{AttrVal, DocSnapshot, Document, FormatProvider};
    use std::collections::BTreeMap;

    type Norm = Vec<(String, String, Vec<(usize, usize, String)>, BTreeMap<String, AttrVal>)>;

    fn norm(snap: &DocSnapshot) -> Norm {
        snap.blocks
            .iter()
            .map(|b| {
                let mut m: Vec<_> = b.marks.iter().map(|x| (x.start, x.end, x.name.clone())).collect();
                m.sort();
                (b.kind.clone(), b.text.clone(), m, b.attrs.clone())
            })
            .collect()
    }

    #[test]
    fn export_import_roundtrips_model() {
        let styles = r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#;
        let body = concat!(
            r#"<text:h text:outline-level="1">Title</text:h>"#,
            r#"<text:p>a <text:span text:style-name="T1">bold</text:span> b</text:p>"#,
            r#"<text:list><text:list-item><text:p>one</text:p></text:list-item>"#,
            r#"<text:list-item><text:p>two</text:p></text:list-item></text:list>"#,
        );
        let odt = minimal_odt_styled(styles, body);

        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let m1 = norm(&d.materialize());

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let c = OdtContainer::read(&exported).unwrap();
        assert_eq!(c.parts[0].0, "mimetype", "exported .odt has mimetype first");
        assert!(c.get("content.xml").is_some());

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let m2 = norm(&d2.materialize());
        assert_eq!(m1, m2, "model round-trips through .odt export then import");
    }

    #[test]
    fn paragraph_props_roundtrip() {
        // A paragraph with alignment + font + size must survive .odt round-trip.
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "styled");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&id, "align", "center");
            dm.set_attr(&id, "fontSize", 22_i64);
            dm.set_attr(&id, "font", "serif");
        }

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let b = &snap.blocks[0];
        assert_eq!(b.text, "styled");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())));
        assert_eq!(b.attrs.get("fontSize"), Some(&AttrVal::Int(22)));
        assert_eq!(b.attrs.get("font"), Some(&AttrVal::Str("serif".into())));
    }

    #[test]
    fn preserves_unmodeled_parts() {
        // Inject a part the codec doesn't model.
        let base = minimal_odt_styled("", "<text:p>hi</text:p>");
        let mut c = OdtContainer::read(&base).unwrap();
        c.set("settings.xml", b"<office:settings>custom</office:settings>".to_vec());
        let odt = c.write().unwrap();

        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let exported = OdtCodec.export(d.as_ref()).unwrap();

        let out = OdtContainer::read(&exported).unwrap();
        assert_eq!(
            out.get("settings.xml"),
            Some(&b"<office:settings>custom</office:settings>"[..]),
            "unmodeled part must round-trip byte-identically"
        );
        assert_eq!(out.parts[0].0, "mimetype", "mimetype still first");
        assert!(out.get("content.xml").is_some(), "content regenerated");
    }
}
