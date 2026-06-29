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

    // Write each image block's blob into the zip as `Pictures/<hash>.<ext>` and
    // collect manifest entries (the `draw:image` hrefs in content.xml match).
    let mut pic_entries: Vec<(String, &'static str)> = Vec::new();
    for b in &snap.blocks {
        if b.kind != "image" {
            continue;
        }
        if let Some(AttrVal::Str(hash)) = b.attrs.get("blob") {
            if let Some(bytes) = snap.blobs.get(hash) {
                let (ext, media) = image_ext_and_media(bytes);
                let path = format!("Pictures/{hash}.{ext}");
                c.set(&path, bytes.clone());
                if !pic_entries.iter().any(|(p, _)| p == &path) {
                    pic_entries.push((path, media));
                }
            }
        }
    }
    if !pic_entries.is_empty() {
        let manifest = c.get("META-INF/manifest.xml").map(|b| b.to_vec()).unwrap_or_else(|| MANIFEST.to_vec());
        c.set("META-INF/manifest.xml", inject_manifest_entries(&manifest, &pic_entries));
    }

    c.set("content.xml", content.into_bytes());
    c.write()
}

/// Detect an image's file extension + ODF media type from its magic bytes.
/// Unknown payloads fall back to a generic binary stream.
fn image_ext_and_media(bytes: &[u8]) -> (&'static str, &'static str) {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        ("png", "image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ("jpg", "image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ("gif", "image/gif")
    } else {
        ("bin", "application/octet-stream")
    }
}

/// The `Pictures/<hash>.<ext>` href an image block's blob is written under.
fn picture_href(hash: &str, bytes: &[u8]) -> String {
    let (ext, _) = image_ext_and_media(bytes);
    format!("Pictures/{hash}.{ext}")
}

/// Insert `manifest:file-entry` rows for `entries` (path, media-type) not already
/// present, just before the closing `</manifest:manifest>`.
fn inject_manifest_entries(manifest: &[u8], entries: &[(String, &'static str)]) -> Vec<u8> {
    let s = String::from_utf8_lossy(manifest);
    let mut inserts = String::new();
    for (path, media) in entries {
        if s.contains(&format!(r#"manifest:full-path="{path}""#)) {
            continue;
        }
        inserts.push_str(&format!(
            r#"<manifest:file-entry manifest:full-path="{path}" manifest:media-type="{media}"/>"#
        ));
    }
    if inserts.is_empty() {
        return manifest.to_vec();
    }
    match s.rfind("</manifest:manifest>") {
        Some(pos) => {
            let mut out = String::with_capacity(s.len() + inserts.len());
            out.push_str(&s[..pos]);
            out.push_str(&inserts);
            out.push_str(&s[pos..]);
            out.into_bytes()
        }
        None => manifest.to_vec(),
    }
}

/// Escape text for XML element content (`svg:title` / `svg:desc`).
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        push_escaped(&mut out, c);
    }
    out
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

/// Build the `draw:frame` XML for an `image` block, or `None` if its blob is
/// absent from the document. The `xlink:href` matches the `Pictures/<hash>.<ext>`
/// part `export()` writes for the same blob.
fn image_frame_xml(b: &BlockView, snap: &DocSnapshot) -> Option<String> {
    let hash = match b.attrs.get("blob") {
        Some(AttrVal::Str(h)) => h,
        _ => return None,
    };
    let bytes = snap.blobs.get(hash)?;
    let href = picture_href(hash, bytes);
    let mut frame = String::from("<draw:frame");
    frame.push_str(&format!(r#" draw:name="{}""#, escape_attr(&b.id)));
    if let Some(AttrVal::Int(w)) = b.attrs.get("width") {
        if *w > 0 {
            frame.push_str(&format!(r#" svg:width="{w}px""#));
        }
    }
    if let Some(AttrVal::Int(h)) = b.attrs.get("height") {
        if *h > 0 {
            frame.push_str(&format!(r#" svg:height="{h}px""#));
        }
    }
    frame.push('>');
    frame.push_str(&format!(r#"<draw:image xlink:href="{}"/>"#, escape_attr(&href)));
    if let Some(AttrVal::Str(alt)) = b.attrs.get("alt") {
        if !alt.is_empty() {
            frame.push_str(&format!("<svg:title>{}</svg:title>", escape_text(alt)));
        }
    }
    frame.push_str("</draw:frame>");
    Some(frame)
}

/// A list item's 0-based nesting level (absent attr → 0).
fn snap_list_level(b: &BlockView) -> i64 {
    match b.attrs.get("level") {
        Some(AttrVal::Int(n)) if *n >= 0 => *n,
        _ => 0,
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
                // Emit a run of consecutive list items as (possibly nested) ODF
                // lists, reconstructing the `text:list`/`text:list-item` nesting
                // from each item's 0-based `level` so depth round-trips. A nested
                // list lives inside its parent item, so items stay open until we
                // know whether the next item is deeper.
                let mut depth: i64 = 0; // number of <text:list> currently open
                let mut item_open = false; // a <text:list-item> awaiting its close
                while i < snap.blocks.len() && snap.blocks[i].kind == "list_item" {
                    let li = &snap.blocks[i];
                    let target = snap_list_level(li) + 1; // lists that should be open
                    if target > depth {
                        // Deeper: open new lists nested inside the current open item.
                        while depth < target {
                            body.push_str("<text:list>");
                            depth += 1;
                        }
                    } else {
                        // Sibling or shallower: close the current item, then unwind
                        // any deeper lists (each wrapped in its parent item).
                        if item_open {
                            body.push_str("</text:list-item>");
                            item_open = false;
                        }
                        while depth > target {
                            body.push_str("</text:list>");
                            depth -= 1;
                            body.push_str("</text:list-item>");
                        }
                    }
                    let sa = style_attr(li);
                    body.push_str(&format!(r#"<text:list-item><text:p{sa}>{}</text:p>"#, render_inline(&li.text, &li.marks, &mut used)));
                    item_open = true;
                    i += 1;
                }
                // Close the final item + all remaining open lists.
                if item_open {
                    body.push_str("</text:list-item>");
                }
                while depth > 0 {
                    body.push_str("</text:list>");
                    depth -= 1;
                    if depth > 0 {
                        body.push_str("</text:list-item>");
                    }
                }
            }
            "image" => {
                // Emit a top-level `draw:frame` referencing the blob written as a
                // `Pictures/<hash>.<ext>` part. Size (svg:width/height in px) and
                // alt (svg:title) round-trip when present.
                if let Some(frame) = image_frame_xml(b, snap) {
                    body.push_str(&frame);
                }
                i += 1;
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

    /// A genuinely valid 1x1 truecolor-RGB PNG (signature + IHDR + IDAT + IEND),
    /// embedded so the image round-trip is self-contained.
    const TINY_PNG: [u8; 69] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
        0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
        0x00, 0x03, 0x01, 0x01, 0x00, 0xf7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
        0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn image_block_roundtrips_through_odt() {
        // Build: paragraph, image (with blob + alt + size), paragraph.
        let mut d = Document::new();
        let p1 = d.insert_block(None, "paragraph");
        d.splice_text(&p1, 0, 0, "before");
        let img = d.insert_block(Some(&p1), "image");
        let p3 = d.insert_block(Some(&img), "paragraph");
        d.splice_text(&p3, 0, 0, "after");
        let hash = d.put_blob(&TINY_PNG);
        {
            let mut dm = d.as_mut();
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "alt", "a tiny red dot");
            dm.set_attr(&img, "width", 64_i64);
            dm.set_attr(&img, "height", 48_i64);
        }

        let exported = OdtCodec.export(d.as_ref()).unwrap();

        // The zip carries the picture + a manifest entry for it.
        let cont = OdtContainer::read(&exported).unwrap();
        let pic = cont
            .parts
            .iter()
            .find(|(n, _)| n.starts_with("Pictures/") && n.ends_with(".png"))
            .expect("Pictures/<hash>.png written");
        assert_eq!(pic.1, &TINY_PNG[..], "picture bytes written verbatim");
        let manifest = std::str::from_utf8(cont.get("META-INF/manifest.xml").unwrap()).unwrap();
        assert!(
            manifest.contains(&pic.0) && manifest.contains("image/png"),
            "picture registered in manifest: {manifest}"
        );
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(content.contains("<draw:frame"), "draw:frame emitted: {content}");
        assert!(content.contains(&format!(r#"xlink:href="{}""#, pic.0)), "image href matches part: {content}");

        // Re-import: order preserved, image survives with bytes + alt + size.
        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        assert_eq!(snap.blocks.len(), 3, "block count preserved: {snap:?}");
        assert_eq!(snap.blocks[0].kind, "paragraph");
        assert_eq!(snap.blocks[0].text, "before");
        assert_eq!(snap.blocks[1].kind, "image", "middle block is the image");
        assert_eq!(snap.blocks[2].kind, "paragraph");
        assert_eq!(snap.blocks[2].text, "after");

        let im = &snap.blocks[1];
        let blob_hash = match im.attrs.get("blob") {
            Some(AttrVal::Str(h)) => h.clone(),
            other => panic!("image block missing blob attr: {other:?}"),
        };
        assert_eq!(blob_hash, hash, "blake3 hash stable across round-trip");
        assert_eq!(im.attrs.get("alt"), Some(&AttrVal::Str("a tiny red dot".into())));
        assert_eq!(im.attrs.get("width"), Some(&AttrVal::Int(64)), "{:?}", im.attrs);
        assert_eq!(im.attrs.get("height"), Some(&AttrVal::Int(48)), "{:?}", im.attrs);

        let bytes = snap.blobs.get(&blob_hash).expect("blob bytes present after round-trip");
        assert_eq!(bytes.as_slice(), &TINY_PNG[..], "image bytes survive the round-trip");
        // And the read-side accessor returns them too.
        assert_eq!(d2.blob(&blob_hash).as_deref(), Some(&TINY_PNG[..]));
    }

    #[test]
    fn nested_list_levels_roundtrip_through_odt() {
        // A list nested as 0,0,1,0 must export as real nested `text:list`s and
        // re-import with the same per-item levels.
        let mut d = Document::new();
        let a = d.insert_block(None, "list_item");
        d.splice_text(&a, 0, 0, "a");
        let b = d.insert_block(Some(&a), "list_item");
        d.splice_text(&b, 0, 0, "b");
        let nested = d.insert_block(Some(&b), "list_item");
        d.splice_text(&nested, 0, 0, "nested");
        d.as_mut().set_attr(&nested, "level", 1_i64);
        let c = d.insert_block(Some(&nested), "list_item");
        d.splice_text(&c, 0, 0, "c");

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        // Export reconstructs nesting: the nested list lives inside item "b".
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(
            content.contains(r#"<text:list-item><text:p>b</text:p><text:list><text:list-item><text:p>nested</text:p></text:list-item></text:list></text:list-item>"#),
            "nested list emitted inside its parent item: {content}"
        );

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let level = |b: &crate::snapshot::BlockView| match b.attrs.get("level") {
            Some(AttrVal::Int(n)) => *n,
            _ => 0i64,
        };
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").map(|b| (b.text.clone(), level(b))).collect();
        assert_eq!(
            items,
            vec![("a".into(), 0i64), ("b".into(), 0), ("nested".into(), 1), ("c".into(), 0)],
            "levels round-trip through nested ODF lists"
        );
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
