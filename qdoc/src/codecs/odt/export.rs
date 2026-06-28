//! Export the canonical model back to a `.odt`. Phase 4 Task 4 generates a
//! fresh minimal ODF from the model (model round-trip). Verbatim preservation of
//! the *original* container's parts + prologue is Task 5 — this exporter is what
//! Task 5 will splice the generated body into.

use super::container::OdtContainer;
use crate::document::MarkSpan;
use crate::format::CodecError;
use crate::model::DocRef;
use crate::snapshot::{AttrVal, DocSnapshot};
use std::collections::BTreeMap;

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

/// Codec-authored automatic *text* style for a mark: `(style name, properties)`.
/// Boolean marks map to fixed styles; valued font marks get a per-value style.
fn mark_style(m: &MarkSpan) -> Option<(String, String)> {
    match m.name.as_str() {
        "strong" => Some(("qdoc-strong".into(), r#"fo:font-weight="bold""#.into())),
        "em" => Some(("qdoc-em".into(), r#"fo:font-style="italic""#.into())),
        "underline" => Some(("qdoc-underline".into(), r#"style:text-underline-style="solid""#.into())),
        "strike" => Some(("qdoc-strike".into(), r#"style:text-line-through-style="solid""#.into())),
        "fontSize" => m.value.as_int().map(|n| (format!("qdoc-fs-{n}"), format!(r#"fo:font-size="{n}pt""#))),
        "fontFamily" => m.value.as_str().map(|f| (format!("qdoc-ff-{f}"), format!(r#"style:font-name="{}""#, font_name(f)))),
        "color" => m.value.as_str().map(|c| (format!("qdoc-color-{}", color_token(c)), format!(r#"fo:color="{}""#, c))),
        _ => None,
    }
}

/// A style-name-safe token for a colour value (`#rrggbb` → `rrggbb`); keeps the
/// generated automatic-style name a valid XML name.
fn color_token(c: &str) -> String {
    c.chars().filter(|ch| ch.is_ascii_alphanumeric()).collect()
}

/// Escape a string for use inside a double-quoted XML attribute value.
fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// The active `link` href at character index `i`, if any. Same-name marks never
/// overlap in automerge's materialised output, so this is unambiguous.
fn link_at(marks: &[MarkSpan], i: usize) -> Option<String> {
    marks
        .iter()
        .find(|m| m.name == "link" && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str().map(|s| s.to_string()))
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

/// Render a block's text with `text:a` hyperlinks (outer) and `text:span` style
/// wrappers (inner) for its marks. The link wrapper and the span set are each
/// closed and re-opened whenever they change, so the output is always
/// well-nested even for overlapping marks.
fn render_inline(text: &str, marks: &[MarkSpan], used: &mut std::collections::BTreeMap<String, String>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut open: Vec<String> = Vec::new();
    let mut cur_link: Option<String> = None;
    let close_spans = |out: &mut String, open: &mut Vec<String>| {
        for _ in 0..open.len() {
            out.push_str("</text:span>");
        }
        open.clear();
    };
    for i in 0..n {
        let link = link_at(marks, i);
        if link != cur_link {
            // A link is the outer element, so close any open spans first, then the
            // old link, then open the new link (spans re-open below).
            close_spans(&mut out, &mut open);
            if cur_link.is_some() {
                out.push_str("</text:a>");
            }
            if let Some(href) = &link {
                out.push_str(&format!(r#"<text:a xlink:href="{}">"#, escape_attr(href)));
            }
            cur_link = link;
        }
        let mut want: Vec<String> = marks
            .iter()
            .filter(|m| i >= m.start && i < m.end)
            .filter_map(|m| {
                mark_style(m).map(|(name, props)| {
                    used.insert(name.clone(), props);
                    name
                })
            })
            .collect();
        want.sort();
        want.dedup();
        if want != open {
            close_spans(&mut out, &mut open);
            for name in &want {
                out.push_str(&format!(r#"<text:span text:style-name="{name}">"#));
                open.push(name.clone());
            }
        }
        push_escaped(&mut out, chars[i]);
    }
    close_spans(&mut out, &mut open);
    if cur_link.is_some() {
        out.push_str("</text:a>");
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

/// Paragraph-properties for a block's alignment, if non-default. (Font is a
/// per-run mark, handled by `render_inline`.)
fn para_props(b: &BlockView) -> Option<(String, String)> {
    let mut pp = String::new();
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
    if pp.is_empty() {
        None
    } else {
        Some((pp, String::new()))
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
    let mut used: BTreeMap<String, String> = BTreeMap::new();
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
                        let cell = cells.iter().find(|cl| snap_cell_rc(cl) == (r, c));
                        // The cell's alignment lives on the inner paragraph in ODF.
                        let sa = cell.map(|cl| style_attr(cl)).unwrap_or_default();
                        let inner = cell.map(|cl| render_inline(&cl.text, &cl.marks, &mut used)).unwrap_or_default();
                        body.push_str(&format!(r#"<table:table-cell><text:p{sa}>{inner}</text:p></table:table-cell>"#));
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
    for (name, props) in &used {
        styles.push_str(&format!(
            r#"<style:style style:name="{name}" style:family="text"><style:text-properties {props}/></style:style>"#
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
        use crate::document::MarkValue;
        // Alignment (paragraph attr) + per-run font marks must survive .odt round-trip.
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "styled");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&id, "align", "center");
            dm.add_mark(&id, 0, 6, "fontSize", 22_i64);
            dm.add_mark(&id, 0, 6, "fontFamily", "serif");
        }

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let b = &snap.blocks[0];
        assert_eq!(b.text, "styled");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())));
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(22)), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "{:?}", b.marks);
    }

    #[test]
    fn hyperlink_roundtrips() {
        use crate::document::MarkValue;
        // A `link` mark exports as `text:a` and re-imports as the same mark+href.
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "go home now");
        d.as_mut().add_mark(&id, 3, 7, "link", "https://qos.dev/");

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let c = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(c.get("content.xml").unwrap()).unwrap();
        assert!(
            content.contains(r#"<text:a xlink:href="https://qos.dev/">home</text:a>"#),
            "exported a text:a element: {content}"
        );

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let b = &d2.materialize().blocks[0];
        assert_eq!(b.text, "go home now");
        let link = b.marks.iter().find(|m| m.name == "link").expect("link survives round-trip");
        assert_eq!((link.start, link.end), (3, 7), "{:?}", b.marks);
        assert_eq!(link.value, MarkValue::Str("https://qos.dev/".into()));
    }

    #[test]
    fn hyperlink_escapes_href() {
        // An href with `&` must be attribute-escaped on export and unescaped on import.
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "link");
        d.as_mut().add_mark(&id, 0, 4, "link", "https://x/?a=1&b=2");

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let c = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(c.get("content.xml").unwrap()).unwrap();
        assert!(content.contains("a=1&amp;b=2"), "ampersand escaped: {content}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let link = d2.materialize().blocks[0].marks.iter().find(|m| m.name == "link").unwrap().value.clone();
        assert_eq!(link, crate::document::MarkValue::Str("https://x/?a=1&b=2".into()));
    }

    #[test]
    fn text_color_roundtrips() {
        use crate::document::MarkValue;
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "red text");
        d.as_mut().add_mark(&id, 0, 3, "color", "#ff0000");

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let b = &d2.materialize().blocks[0];
        assert_eq!(b.text, "red text");
        assert!(
            b.marks.iter().any(|m| m.name == "color" && m.start == 0 && m.end == 3 && m.value == MarkValue::Str("#ff0000".into())),
            "color survives round-trip: {:?}",
            b.marks
        );
    }

    #[test]
    fn link_and_style_marks_nest_well() {
        use crate::document::MarkValue;
        // A bold, coloured hyperlink: link (outer text:a) wraps the style spans.
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "click");
        {
            let mut dm = d.as_mut();
            dm.add_mark(&id, 0, 5, "link", "http://q/");
            dm.add_mark(&id, 0, 5, "strong", true);
            dm.add_mark(&id, 0, 5, "color", "#0000ff");
        }
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let b = &d2.materialize().blocks[0];
        assert_eq!(b.text, "click");
        for name in ["link", "strong", "color"] {
            assert!(b.marks.iter().any(|m| m.name == name && m.start == 0 && m.end == 5), "{name} survives: {:?}", b.marks);
        }
        assert_eq!(b.marks.iter().find(|m| m.name == "link").unwrap().value, MarkValue::Str("http://q/".into()));
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
