//! Export the canonical model back to a `.odt`. Phase 4 Task 4 generates a
//! fresh minimal ODF from the model (model round-trip). Verbatim preservation of
//! the *original* container's parts + prologue is Task 5 — this exporter is what
//! Task 5 will splice the generated body into.

use super::container::OdtContainer;
use crate::document::MarkSpan;
use crate::format::CodecError;
use crate::model::DocRef;
use crate::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME, STYLE_SIZE, STYLE_SPACE_AFTER,
    STYLE_SPACE_BEFORE,
};
use crate::snapshot::{AttrVal, DocSnapshot};
use std::collections::{BTreeMap, BTreeSet};

const MIMETYPE: &[u8] = b"application/vnd.oasis.opendocument.text";
const MANIFEST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
<manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>
<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;

pub(crate) fn export(doc: DocRef) -> Result<Vec<u8>, CodecError> {
    let snap = doc.materialize();
    let preserved = crate::model::preserved_parts(doc.doc, doc.c);
    // Named styles already defined in a preserved `styles.xml` must not be
    // re-emitted into `content.xml` (that would duplicate the definition); we
    // only reference them. Freshly-authored styles (not in any styles.xml) are
    // emitted into content.xml's automatic-styles.
    let existing_style_names = preserved
        .iter()
        .find(|(k, _)| k == "odt/part/styles.xml")
        .map(|(_, b)| style_names_in(b))
        .unwrap_or_default();
    let content = build_content_xml(&snap, &existing_style_names);

    let mut c = OdtContainer { parts: Vec::new() };
    // Re-emit every preserved part verbatim (styles.xml, settings.xml, images,
    // the original manifest/mimetype, …).
    let mut had_mimetype = false;
    let mut had_manifest = false;
    let mut had_meta = false;
    let mut had_styles = false;
    let mut original_meta: Option<Vec<u8>> = None;
    let mut original_styles: Option<Vec<u8>> = None;
    for (key, bytes) in preserved {
        if let Some(name) = key.strip_prefix("odt/part/") {
            had_mimetype |= name == "mimetype";
            had_manifest |= name == "META-INF/manifest.xml";
            if name == "meta.xml" {
                had_meta = true;
                original_meta = Some(bytes.clone());
            }
            if name == "styles.xml" {
                had_styles = true;
                original_styles = Some(bytes.clone());
            }
            c.set(name, bytes);
        }
    }
    // Page header/footer → `styles.xml` master-page: splice the regenerated
    // `style:header`/`style:footer` into a copy of the original `styles.xml` (or
    // synthesise a minimal one if the source had none). Only runs when the document
    // actually has a header or footer, so a doc with neither leaves `styles.xml`
    // untouched (or absent).
    if let Some(styles_xml) = super::headers::export_master_headers(&doc, original_styles.as_deref()) {
        let is_new = !had_styles;
        c.set("styles.xml", styles_xml);
        if is_new {
            had_styles = true;
        }
    }
    // Document properties → `meta.xml`: regenerate the part from the canonical
    // properties, merging in any unmodeled `office:meta` children from the
    // preserved original so they survive. Created when the doc has properties even
    // if the source had no meta.xml (so freshly-authored properties export).
    if let Some(meta_xml) = super::meta::export_meta(&doc, original_meta.as_deref()) {
        let is_new = !had_meta;
        c.set("meta.xml", meta_xml);
        if is_new {
            had_meta = true;
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
    // collect manifest entries (the `draw:image` hrefs in content.xml match). A
    // freshly-created `meta.xml` also needs a manifest entry.
    let mut pic_entries: Vec<(String, &'static str)> = Vec::new();
    if had_meta {
        pic_entries.push(("meta.xml".to_string(), "text/xml"));
    }
    if had_styles {
        pic_entries.push(("styles.xml".to_string(), "text/xml"));
    }
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

/// A note-id → `&NoteView` lookup for resolving body reference markers on export.
type NoteIndex<'a> = std::collections::BTreeMap<String, &'a crate::snapshot::NoteView>;

fn note_index(snap: &DocSnapshot) -> NoteIndex<'_> {
    snap.notes.iter().map(|n| (n.id.clone(), n)).collect()
}

/// A comment-id → `&CommentView` lookup for emitting `office:annotation`(+`-end`) at
/// a commented range's boundaries on export.
type CommentIndex<'a> = std::collections::BTreeMap<String, &'a crate::snapshot::CommentView>;

fn comment_index(snap: &DocSnapshot) -> CommentIndex<'_> {
    snap.comments.iter().map(|c| (c.id.clone(), c)).collect()
}

/// The note id of a `note-ref` mark covering exactly `[i, i+1)`, if any.
fn note_ref_at(marks: &[MarkSpan], i: usize) -> Option<String> {
    marks
        .iter()
        .find(|m| m.name == crate::schema::MARK_NOTE_REF && m.start == i && m.end == i + 1)
        .and_then(|m| m.value.as_str().map(|s| s.to_string()))
}

/// The comment ids whose anchoring `comment` mark starts exactly at `i` (sorted for
/// deterministic output) / ends exactly at `i` (i.e. `mark.end == i`).
fn comment_starts_at(marks: &[MarkSpan], i: usize) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .filter(|m| m.name == crate::schema::MARK_COMMENT && m.start == i)
        .filter_map(|m| m.value.as_str().map(|s| s.to_string()))
        .collect();
    v.sort();
    v
}

fn comment_ends_at(marks: &[MarkSpan], i: usize) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .filter(|m| m.name == crate::schema::MARK_COMMENT && m.end == i)
        .filter_map(|m| m.value.as_str().map(|s| s.to_string()))
        .collect();
    v.sort();
    v
}

/// The tracked-change ids (any `ins`/`del` mark) starting / ending exactly at `i`,
/// sorted for deterministic output.
fn change_starts_at(marks: &[MarkSpan], i: usize) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .filter(|m| (m.name == crate::schema::MARK_INS || m.name == crate::schema::MARK_DEL) && m.start == i)
        .filter_map(|m| m.value.as_str().map(|s| s.to_string()))
        .collect();
    v.sort();
    v
}

fn change_ends_at(marks: &[MarkSpan], i: usize) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .filter(|m| (m.name == crate::schema::MARK_INS || m.name == crate::schema::MARK_DEL) && m.end == i)
        .filter_map(|m| m.value.as_str().map(|s| s.to_string()))
        .collect();
    v.sort();
    v
}

/// Render a block's text with `text:a` hyperlinks (outer) and `text:span` style
/// wrappers (inner) for its marks. The link wrapper and the span set are each
/// closed and re-opened whenever they change, so the output is always
/// well-nested even for overlapping marks. A placeholder covered by a `note-ref`
/// mark is emitted as the referenced inline `text:note` (advancing `note_ctr`). A
/// `comment` mark emits an `office:annotation` at its start and an
/// `office:annotation-end` at its end (both outside the span/link wrappers so the
/// output stays well-nested).
fn render_inline(
    text: &str,
    marks: &[MarkSpan],
    used: &mut std::collections::BTreeMap<String, String>,
    notes: &NoteIndex,
    note_ctr: &mut usize,
    comments: &CommentIndex,
) -> String {
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
    // Emit annotation boundary markers at `i`, closing spans/link first so the
    // annotation element is a clean sibling of the surrounding inline content.
    let emit_comment_boundaries = |out: &mut String, open: &mut Vec<String>, cur_link: &mut Option<String>, i: usize| {
        let ends = comment_ends_at(marks, i);
        let starts = comment_starts_at(marks, i);
        if ends.is_empty() && starts.is_empty() {
            return;
        }
        close_spans(out, open);
        if cur_link.is_some() {
            out.push_str("</text:a>");
            *cur_link = None;
        }
        // Ends before starts so an adjacent comment's end closes before the next opens.
        for id in ends {
            if let Some(c) = comments.get(&id) {
                out.push_str(&super::comments::render_annotation_end(c));
            }
        }
        for id in starts {
            if let Some(c) = comments.get(&id) {
                out.push_str(&super::comments::render_annotation_start(c));
            }
        }
    };
    // Emit tracked-change `text:change-end`/`text:change-start` markers at `i`, closing
    // spans/link first so each marker is a clean sibling of the surrounding content.
    let emit_change_boundaries = |out: &mut String, open: &mut Vec<String>, cur_link: &mut Option<String>, i: usize| {
        let ends = change_ends_at(marks, i);
        let starts = change_starts_at(marks, i);
        if ends.is_empty() && starts.is_empty() {
            return;
        }
        close_spans(out, open);
        if cur_link.is_some() {
            out.push_str("</text:a>");
            *cur_link = None;
        }
        // Ends before starts so an adjacent change's end closes before the next opens.
        for id in ends {
            out.push_str(&super::changes::render_change_end(&id));
        }
        for id in starts {
            out.push_str(&super::changes::render_change_start(&id));
        }
    };
    for i in 0..n {
        emit_comment_boundaries(&mut out, &mut open, &mut cur_link, i);
        emit_change_boundaries(&mut out, &mut open, &mut cur_link, i);
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
        // A placeholder carrying a `note-ref` mark renders as the inline note.
        if chars[i] == crate::schema::FIELD_PLACEHOLDER {
            if let Some(note_id) = note_ref_at(marks, i) {
                if let Some(note) = notes.get(&note_id) {
                    *note_ctr += 1;
                    out.push_str(&super::notes::render_inline_note(note, *note_ctr));
                }
                // A note-ref placeholder is consumed by the note; emit nothing else.
                continue;
            }
        }
        push_escaped(&mut out, chars[i]);
    }
    close_spans(&mut out, &mut open);
    if cur_link.is_some() {
        out.push_str("</text:a>");
    }
    // A comment ending exactly at the block's end (`mark.end == n`) emits its
    // annotation-end after the last char.
    for id in comment_ends_at(marks, n) {
        if let Some(c) = comments.get(&id) {
            out.push_str(&super::comments::render_annotation_end(c));
        }
    }
    // A tracked change ending exactly at the block's end emits its change-end marker.
    for id in change_ends_at(marks, n) {
        out.push_str(&super::changes::render_change_end(&id));
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
            "justify" => "justify",
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

/// Whether a list item is ordered (numbered).
fn snap_list_ordered(b: &BlockView) -> bool {
    matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)))
}

/// The model numbering format string of a list item (default `decimal`).
fn snap_num_format(b: &BlockView) -> &str {
    match b.attrs.get("num_format") {
        Some(AttrVal::Str(s)) => s.as_str(),
        _ => "decimal",
    }
}

/// The numbering start value of a list item (default 1).
fn snap_num_start(b: &BlockView) -> i64 {
    match b.attrs.get("num_start") {
        Some(AttrVal::Int(n)) => *n,
        _ => 1,
    }
}

/// Whether a list item restarts numbering (vs continuing).
fn snap_num_restart(b: &BlockView) -> bool {
    matches!(b.attrs.get("num_restart"), Some(AttrVal::Bool(true)))
}

/// Map the model's canonical numbering format onto an ODF `style:num-format`
/// value (inverse of `styles::map_num_format`).
fn odf_num_format(fmt: &str) -> &'static str {
    match fmt {
        "lower-roman" => "i",
        "upper-roman" => "I",
        "lower-alpha" => "a",
        "upper-alpha" => "A",
        _ => "1",
    }
}

/// A `text:list-style` definition reconstructing a level-0 numbering (format +
/// start) so an ordered list re-imports with its numbering intact. `name` is the
/// generated automatic-style name referenced by the `text:list`.
fn list_style_def_xml(name: &str, fmt: &str, start: i64) -> String {
    let nf = odf_num_format(fmt);
    let sv = if start != 1 { format!(r#" text:start-value="{start}""#) } else { String::new() };
    format!(
        r#"<text:list-style style:name="{name}"><text:list-level-style-number text:level="1" style:num-format="{nf}"{sv}/></text:list-style>"#
    )
}

/// A registered reconstructed list-style: its generated name + the `(format,
/// start)` key it encodes + its full `text:list-style` XML.
struct ListStyle {
    name: String,
    key: (String, i64),
    xml: String,
}

/// The opening `<text:list …>` tag for the list `li` lives in. An ordered item
/// with a non-default format and/or start gets a reconstructed `text:list-style`
/// (registered in `list_styles`, deduped on `(format, start)`) referenced via
/// `text:style-name`, plus `text:start-value` (a restart) or
/// `text:continue-numbering` on the element.
fn open_list_tag(li: &BlockView, list_styles: &mut Vec<ListStyle>) -> String {
    if !snap_list_ordered(li) {
        return "<text:list>".to_string();
    }
    let fmt = snap_num_format(li);
    let start = snap_num_start(li);
    // Plain decimal-from-1 ordered lists need no explicit style (LibreOffice's
    // default), keeping the output minimal.
    if fmt == "decimal" && start == 1 {
        return "<text:list>".to_string();
    }
    let key = (fmt.to_string(), start);
    let name = match list_styles.iter().find(|s| s.key == key) {
        Some(s) => s.name.clone(),
        None => {
            let name = format!("Lqdoc{}", list_styles.len() + 1);
            let xml = list_style_def_xml(&name, fmt, start);
            list_styles.push(ListStyle { name: name.clone(), key, xml });
            name
        }
    };
    let mut tag = format!(r#"<text:list text:style-name="{name}""#);
    if start != 1 && snap_num_restart(li) {
        tag.push_str(&format!(r#" text:start-value="{start}""#));
    } else if start != 1 {
        // A non-default start that continues: mark it continuing so the importer
        // doesn't treat it as a fresh restart.
        tag.push_str(r#" text:continue-numbering="true""#);
    }
    tag.push('>');
    tag
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

/// Get-or-create a paragraph automatic style for `(parent, paragraph-props,
/// text-props)`, returning its `P{n}` name. Dedups identical styles.
fn register_auto(styles: &mut Vec<(Option<String>, String, String)>, key: (Option<String>, String, String)) -> String {
    if let Some(i) = styles.iter().position(|p| p == &key) {
        format!("P{}", i + 1)
    } else {
        styles.push(key);
        format!("P{}", styles.len())
    }
}

/// The ` text:style-name="…"` attribute for a block, resolving an explicit named
/// style (from the styles container) and/or an alignment override. Registers any
/// referenced named style + derived automatic style as a side effect.
fn resolve_para_style(
    b: &BlockView,
    styles: &BTreeMap<String, BTreeMap<String, AttrVal>>,
    auto: &mut Vec<(Option<String>, String, String)>,
    named_refs: &mut BTreeSet<String>,
) -> String {
    let named = match b.attrs.get("style") {
        Some(AttrVal::Str(s)) if styles.contains_key(s) => Some(s.clone()),
        _ => None,
    };
    let align = para_props(b).map(|(pp, _)| pp);
    let name = match (named, align) {
        (None, None) => return String::new(),
        (None, Some(pp)) => register_auto(auto, (None, pp, String::new())),
        (Some(id), None) => {
            let enc = super::styles::encode_style_name(&id);
            named_refs.insert(id);
            enc
        }
        (Some(id), Some(pp)) => {
            let parent = super::styles::encode_style_name(&id);
            named_refs.insert(id);
            register_auto(auto, (Some(parent), pp, String::new()))
        }
    };
    format!(r#" text:style-name="{name}""#)
}

/// Format a float for ODF: integral values without a trailing `.0`.
fn fmt_num(f: f64) -> String {
    if f.fract() == 0.0 {
        format!("{}", f as i64)
    } else {
        let s = format!("{f:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Style names (`style:name="…"`) already declared in a preserved `styles.xml`,
/// so the exporter can reference rather than redefine them.
fn style_names_in(xml: &[u8]) -> BTreeSet<String> {
    let s = String::from_utf8_lossy(xml);
    let mut out = BTreeSet::new();
    let needle = "style:name=\"";
    let mut rest = s.as_ref();
    while let Some(p) = rest.find(needle) {
        rest = &rest[p + needle.len()..];
        if let Some(end) = rest.find('"') {
            out.insert(rest[..end].to_string());
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    out
}

/// An ODF `style:style` definition (family paragraph) for a CRDT styles-container
/// style, so LibreOffice renders the document's named styles.
fn named_style_def_xml(id: &str, props: &BTreeMap<String, AttrVal>) -> String {
    let name = super::styles::encode_style_name(id);
    let getf = |k: &str| match props.get(k) {
        Some(AttrVal::Float(f)) => Some(*f),
        Some(AttrVal::Int(n)) => Some(*n as f64),
        _ => None,
    };
    let gets = |k: &str| match props.get(k) {
        Some(AttrVal::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    let getb = |k: &str| matches!(props.get(k), Some(AttrVal::Bool(true)));

    let mut tp = String::new();
    if getb(STYLE_BOLD) {
        tp.push_str(r#" fo:font-weight="bold""#);
    }
    if getb(STYLE_ITALIC) {
        tp.push_str(r#" fo:font-style="italic""#);
    }
    if let Some(sz) = getf(STYLE_SIZE) {
        tp.push_str(&format!(r#" fo:font-size="{}pt""#, fmt_num(sz)));
    }
    if let Some(f) = gets(STYLE_FONT) {
        tp.push_str(&format!(r#" style:font-name="{}""#, font_name(f)));
    }
    if let Some(c) = gets(STYLE_COLOR) {
        tp.push_str(&format!(r#" fo:color="{}""#, escape_attr(c)));
    }

    let mut pp = String::new();
    if let Some(a) = gets(STYLE_ALIGN) {
        let v = match a {
            "center" => "center",
            "right" => "end",
            "justify" => "justify",
            _ => "",
        };
        if !v.is_empty() {
            pp.push_str(&format!(r#" fo:text-align="{v}""#));
        }
    }
    if let Some(v) = getf(STYLE_SPACE_BEFORE) {
        if v != 0.0 {
            pp.push_str(&format!(r#" fo:margin-top="{}px""#, fmt_num(v)));
        }
    }
    if let Some(v) = getf(STYLE_SPACE_AFTER) {
        if v != 0.0 {
            pp.push_str(&format!(r#" fo:margin-bottom="{}px""#, fmt_num(v)));
        }
    }
    if let Some(v) = getf(STYLE_INDENT) {
        if v != 0.0 {
            pp.push_str(&format!(r#" fo:margin-left="{}px""#, fmt_num(v)));
        }
    }
    let display = gets(STYLE_NAME)
        .map(|n| format!(r#" style:display-name="{}""#, escape_attr(n)))
        .unwrap_or_default();
    format!(
        r#"<style:style style:name="{name}" style:family="paragraph"{display}><style:paragraph-properties{pp}/><style:text-properties{tp}/></style:style>"#
    )
}

use crate::snapshot::BlockView;

fn build_content_xml(snap: &DocSnapshot, existing_style_names: &BTreeSet<String>) -> String {
    let mut body = String::new();
    let mut used: BTreeMap<String, String> = BTreeMap::new();
    let mut pstyles: Vec<(Option<String>, String, String)> = Vec::new();
    let mut named_refs: BTreeSet<String> = BTreeSet::new();
    // Reconstructed `text:list-style`s for ordered lists with non-default
    // numbering (format/start), emitted into automatic-styles.
    let mut list_styles: Vec<ListStyle> = Vec::new();
    let list_styles = &mut list_styles;
    // Footnotes/endnotes referenced from body text, numbered 1.. in reference order.
    let notes = note_index(snap);
    let mut note_ctr = 0usize;
    let note_ctr = &mut note_ctr;
    // Comments/annotations anchored to body ranges, emitted at each range's boundaries.
    let comments = comment_index(snap);
    let comments = &comments;
    // Tracked changes: declared up front in a `text:tracked-changes` block; the body
    // references each by `text:change-start`/`-end` markers (emitted in `render_inline`).
    let tracked_changes = super::changes::render_tracked_changes(&snap.changes);
    let mut style_attr = |b: &BlockView| -> String { resolve_para_style(b, &snap.styles, &mut pstyles, &mut named_refs) };
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
                    render_inline(&b.text, &b.marks, &mut used, &notes, note_ctr, comments)
                ));
                i += 1;
            }
            "list_item" => {
                // Emit a run of consecutive list items as (possibly nested) ODF
                // lists, reconstructing the `text:list`/`text:list-item` nesting
                // from each item's 0-based `level` so depth round-trips. A nested
                // list lives inside its parent item, so items stay open until we
                // know whether the next item is deeper. The list opening the run of
                // a given level carries a reconstructed `text:list-style` reference
                // (format + start) when the item is ordered with non-default
                // numbering, so the numbering survives the round-trip.
                let mut depth: i64 = 0; // number of <text:list> currently open
                let mut item_open = false; // a <text:list-item> awaiting its close
                while i < snap.blocks.len() && snap.blocks[i].kind == "list_item" {
                    let li = &snap.blocks[i];
                    let target = snap_list_level(li) + 1; // lists that should be open
                    if target > depth {
                        // Deeper: open new lists nested inside the current open item.
                        // The innermost (last) list opened is the one `li` lives in,
                        // so it gets `li`'s numbering reference.
                        while depth < target {
                            depth += 1;
                            let open = if depth == target {
                                open_list_tag(li, list_styles)
                            } else {
                                "<text:list>".to_string()
                            };
                            body.push_str(&open);
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
                    body.push_str(&format!(r#"<text:list-item><text:p{sa}>{}</text:p>"#, render_inline(&li.text, &li.marks, &mut used, &notes, note_ctr, comments)));
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
                        let inner = cell.map(|cl| render_inline(&cl.text, &cl.marks, &mut used, &notes, note_ctr, comments)).unwrap_or_default();
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
                    render_inline(&b.text, &b.marks, &mut used, &notes, note_ctr, comments)
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
    // Named (common) styles referenced by a block's `style` attr — emitted unless
    // already declared in a preserved styles.xml. These make the document's CRDT
    // styles render in LibreOffice as real ODF named styles.
    for id in &named_refs {
        if existing_style_names.contains(&super::styles::encode_style_name(id)) {
            continue;
        }
        if let Some(props) = snap.styles.get(id) {
            styles.push_str(&named_style_def_xml(id, props));
        }
    }
    for (idx, (parent, pp, tp)) in pstyles.iter().enumerate() {
        let parent_attr = parent.as_ref().map(|p| format!(r#" style:parent-style-name="{p}""#)).unwrap_or_default();
        styles.push_str(&format!(
            r#"<style:style style:name="P{}" style:family="paragraph"{parent_attr}><style:paragraph-properties{pp}/><style:text-properties{tp}/></style:style>"#,
            idx + 1
        ));
    }
    // Reconstructed list-styles (numbering format + start) for ordered lists.
    for ls in list_styles.iter() {
        styles.push_str(&ls.xml);
    }

    // Declare the common ODF namespaces so re-emitted fidelity atoms (tables,
    // drawings, …) stay well-formed. Truly exotic namespaces are only fully
    // preserved once the original content prologue is carried verbatim (future).
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0">
<office:automatic-styles>{styles}</office:automatic-styles>
<office:body><office:text>{tracked_changes}{body}</office:text></office:body>
</office:document-content>"#
    )
}

#[cfg(test)]
mod tests {
    use super::super::container::{minimal_odt, minimal_odt_styled, OdtContainer};
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

    /// Normalize a region's blocks to `(text, marks-with-values)`, dropping the
    /// volatile per-import block id so two independent imports compare equal.
    fn norm_region(blocks: &[crate::snapshot::BlockView]) -> Vec<(String, Vec<(usize, usize, String, crate::document::MarkValue)>)> {
        blocks
            .iter()
            .map(|b| {
                let mut m: Vec<_> = b.marks.iter().map(|x| (x.start, x.end, x.name.clone(), x.value.clone())).collect();
                m.sort();
                (b.text.clone(), m)
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
    fn colored_serif_20pt_justified_run_roundtrips_through_odt() {
        use crate::document::MarkValue;
        // The headline task: a coloured + serif + 20pt run in a justified
        // paragraph must round-trip export→import with every attribute intact.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "styled");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&p, "align", "justify");
            dm.add_mark(&p, 0, 6, "color", "#3366cc");
            dm.add_mark(&p, 0, 6, "fontFamily", "serif");
            dm.add_mark(&p, 0, 6, "fontSize", 20_i64);
        }
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(content.contains(r#"fo:text-align="justify""#), "justify → fo:text-align justify: {content}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let b = &d2.materialize().blocks[0];
        assert_eq!(b.text, "styled");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("justify".into())), "justify survives: {:?}", b.attrs);
        assert!(b.marks.iter().any(|m| m.name == "color" && m.value == MarkValue::Str("#3366cc".into())), "colour: {:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "serif: {:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)), "20pt: {:?}", b.marks);
    }

    #[test]
    fn list_numbering_format_and_start_roundtrip_through_odt() {
        // An upper-roman ordered list starting at 3: the format + start must
        // survive an .odt export then re-import (fidelity preservation — the editor
        // renders decimal, but the numbering metadata must not be lost).
        let mut d = Document::new();
        let a = d.insert_block(None, "list_item");
        d.splice_text(&a, 0, 0, "three");
        let b = d.insert_block(Some(&a), "list_item");
        d.splice_text(&b, 0, 0, "four");
        {
            let mut dm = d.as_mut();
            for id in [&a, &b] {
                dm.set_attr(id, "ordered", true);
                dm.set_attr(id, "num_format", "upper-roman");
                dm.set_attr(id, "num_start", 3_i64);
                dm.set_attr(id, "num_restart", true);
            }
        }

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(content.contains(r#"style:num-format="I""#), "upper-roman list-style emitted: {content}");
        assert!(content.contains(r#"text:start-value="3""#), "start value emitted: {content}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "list_item").collect();
        assert_eq!(items.len(), 2, "{snap:?}");
        for it in &items {
            assert_eq!(it.attrs.get("ordered"), Some(&AttrVal::Bool(true)), "still ordered: {:?}", it.attrs);
            assert_eq!(it.attrs.get("num_format"), Some(&AttrVal::Str("upper-roman".into())), "format survives: {:?}", it.attrs);
            assert_eq!(it.attrs.get("num_start"), Some(&AttrVal::Int(3)), "start survives: {:?}", it.attrs);
        }

        // Idempotent: a second round-trip preserves the same numbering attrs.
        let exported2 = OdtCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        OdtCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(norm(&d2.materialize()), norm(&d3.materialize()), "list-numbering model stable across a second round-trip");
    }

    #[test]
    fn named_paragraph_style_roundtrips_through_odt() {
        // A paragraph that references a custom CRDT-native named style must export
        // as a real ODF `style:style` (family paragraph) + `text:style-name`, and
        // re-import with the style intact and still applied to the block.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "fancy");
        {
            let mut dm = d.as_mut();
            let mut props = BTreeMap::new();
            props.insert("name".to_string(), AttrVal::Str("Fancy".into()));
            props.insert("font".to_string(), AttrVal::Str("serif".into()));
            props.insert("size".to_string(), AttrVal::Float(20.0));
            props.insert("bold".to_string(), AttrVal::Bool(true));
            props.insert("italic".to_string(), AttrVal::Bool(true));
            props.insert("color".to_string(), AttrVal::Str("#ff0000".into()));
            props.insert("align".to_string(), AttrVal::Str("center".into()));
            props.insert("space_after".to_string(), AttrVal::Float(12.0));
            dm.set_style("Fancy", &props);
            dm.set_attr(&p, "style", "Fancy");
        }

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(content.contains(r#"text:style-name="Fancy""#), "paragraph references the named style: {content}");
        assert!(
            content.contains(r#"<style:style style:name="Fancy" style:family="paragraph""#),
            "named style defined as ODF style:style: {content}"
        );
        assert!(content.contains(r#"fo:font-weight="bold""#) && content.contains(r#"fo:font-size="20pt""#), "props emitted: {content}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let b = &snap.blocks[0];
        assert_eq!(b.text, "fancy");
        assert_eq!(b.attrs.get("style"), Some(&AttrVal::Str("Fancy".into())), "block keeps its style ref: {:?}", b.attrs);

        let st = snap.styles.get("Fancy").expect("style present in container after round-trip");
        assert_eq!(st.get("name"), Some(&AttrVal::Str("Fancy".into())));
        assert_eq!(st.get("font"), Some(&AttrVal::Str("serif".into())));
        assert_eq!(st.get("size"), Some(&AttrVal::Float(20.0)));
        assert_eq!(st.get("bold"), Some(&AttrVal::Bool(true)));
        assert_eq!(st.get("italic"), Some(&AttrVal::Bool(true)));
        assert_eq!(st.get("color"), Some(&AttrVal::Str("#ff0000".into())));
        assert_eq!(st.get("align"), Some(&AttrVal::Str("center".into())));
        assert_eq!(st.get("space_after"), Some(&AttrVal::Float(12.0)));

        // The round-trip is idempotent: exporting d2 yields the same model again.
        let exported2 = OdtCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        OdtCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(norm(&d2.materialize()), norm(&d3.materialize()), "named-style model is stable across a second round-trip");
    }

    #[test]
    fn document_properties_roundtrip_through_odt() {
        // Title/author/subject/keywords/dates/generator set on a fresh doc must
        // export into meta.xml and re-import as the same properties.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "body");
        d.set_doc_prop("title", "My Title");
        d.set_doc_prop("author", "Ada Lovelace");
        d.set_doc_prop("subject", "Analytical Engine");
        d.set_doc_prop("keywords", "computing, history");
        d.set_doc_prop("created", "2020-01-02T03:04:05");
        d.set_doc_prop("modified", "2021-06-07T08:09:10");
        d.set_doc_prop("generator", "qdoc/1.0");

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        // The meta.xml part carries the dc:/meta: fields.
        let cont = OdtContainer::read(&exported).unwrap();
        let meta = std::str::from_utf8(cont.get("meta.xml").expect("meta.xml written")).unwrap();
        assert!(meta.contains("<dc:title>My Title</dc:title>"), "title in meta.xml: {meta}");
        assert!(meta.contains("<dc:creator>Ada Lovelace</dc:creator>"), "creator: {meta}");
        assert!(meta.contains("<dc:subject>Analytical Engine</dc:subject>"), "subject: {meta}");
        assert!(meta.contains("<meta:keyword>computing, history</meta:keyword>"), "keyword: {meta}");
        assert!(meta.contains("<meta:creation-date>2020-01-02T03:04:05</meta:creation-date>"), "created: {meta}");
        assert!(meta.contains("<dc:date>2021-06-07T08:09:10</dc:date>"), "modified: {meta}");
        assert!(meta.contains("<meta:generator>qdoc/1.0</meta:generator>"), "generator: {meta}");
        // A fresh meta.xml is registered in the manifest.
        let manifest = std::str::from_utf8(cont.get("META-INF/manifest.xml").unwrap()).unwrap();
        assert!(manifest.contains("meta.xml"), "meta.xml registered in manifest: {manifest}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let props = d2.doc_props();
        assert_eq!(props.get("title").map(String::as_str), Some("My Title"));
        assert_eq!(props.get("author").map(String::as_str), Some("Ada Lovelace"));
        assert_eq!(props.get("subject").map(String::as_str), Some("Analytical Engine"));
        assert_eq!(props.get("keywords").map(String::as_str), Some("computing, history"));
        assert_eq!(props.get("created").map(String::as_str), Some("2020-01-02T03:04:05"));
        assert_eq!(props.get("modified").map(String::as_str), Some("2021-06-07T08:09:10"));
        assert_eq!(props.get("generator").map(String::as_str), Some("qdoc/1.0"));
        // Body content still intact alongside the metadata.
        assert_eq!(d2.materialize().blocks[0].text, "body");

        // Idempotent across a second round-trip.
        let exported2 = OdtCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        OdtCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(d2.doc_props(), d3.doc_props(), "properties stable across a second round-trip");
    }

    #[test]
    fn meta_xml_preserves_unmodeled_fields_and_updates_modeled() {
        // An imported meta.xml with an unmodeled field (editing-cycles) plus an
        // existing title: the unmodeled field survives and the title updates.
        let meta = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:meta><dc:title>Old</dc:title><meta:editing-cycles>7</meta:editing-cycles></office:meta></office:document-meta>"#;
        let base = minimal_odt_styled("", "<text:p>hi</text:p>");
        let mut c = OdtContainer::read(&base).unwrap();
        c.set("meta.xml", meta.as_bytes().to_vec());
        let odt = c.write().unwrap();

        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        assert_eq!(d.doc_prop("title").as_deref(), Some("Old"), "title imported");
        // Change the title; keywords are new.
        d.set_doc_prop("title", "New");
        d.set_doc_prop("keywords", "k");

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let out = OdtContainer::read(&exported).unwrap();
        let meta_out = std::str::from_utf8(out.get("meta.xml").unwrap()).unwrap();
        assert!(meta_out.contains("<dc:title>New</dc:title>"), "title updated: {meta_out}");
        assert!(!meta_out.contains("<dc:title>Old</dc:title>"), "old title gone: {meta_out}");
        assert!(meta_out.contains("editing-cycles>7</"), "unmodeled field preserved: {meta_out}");
        assert!(meta_out.contains("<meta:keyword>k</meta:keyword>"), "new keyword emitted: {meta_out}");
    }

    #[test]
    fn no_meta_part_when_no_properties_and_none_imported() {
        // A from-scratch doc with no properties must not synthesise a meta.xml.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "plain");
        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        assert!(cont.get("meta.xml").is_none(), "no metadata part synthesised for a doc with no properties");
    }

    #[test]
    fn header_footer_with_page_number_roundtrip_through_odt() {
        use crate::document::MarkValue;
        use crate::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_FIELD};
        // A header "Title" + a footer with a page-number field must export into the
        // master-page in styles.xml and re-import as the same header/footer + field.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "body");
        let h = d.insert_header_block(None, "paragraph");
        d.splice_header_text(&h, 0, 0, "Title");
        let f = d.insert_footer_block(None, "paragraph");
        d.splice_footer_text(&f, 0, 0, "Page ");
        d.insert_footer_page_number(&f, 5);

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        // styles.xml carries the master-page header/footer + a page-number field.
        let cont = OdtContainer::read(&exported).unwrap();
        let styles = std::str::from_utf8(cont.get("styles.xml").expect("styles.xml written")).unwrap();
        assert!(styles.contains("<style:header>") && styles.contains("Title"), "header in master-page: {styles}");
        assert!(styles.contains("<style:footer>"), "footer in master-page: {styles}");
        assert!(styles.contains("<text:page-number/>"), "page-number field emitted: {styles}");
        // The freshly-created styles.xml is registered in the manifest.
        let manifest = std::str::from_utf8(cont.get("META-INF/manifest.xml").unwrap()).unwrap();
        assert!(manifest.contains("styles.xml"), "styles.xml registered in manifest: {manifest}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        // Body intact.
        assert_eq!(d2.materialize().blocks[0].text, "body");
        // Header survives.
        let header = d2.header();
        assert_eq!(header.len(), 1, "header block present: {header:?}");
        assert_eq!(header[0].text, "Title");
        // Footer + field survive.
        let footer = d2.footer();
        assert_eq!(footer.len(), 1, "footer block present: {footer:?}");
        assert_eq!(footer[0].text, format!("Page {}", FIELD_PLACEHOLDER), "placeholder kept: {:?}", footer[0].text);
        let field = footer[0].marks.iter().find(|m| m.name == MARK_FIELD).expect("page-number field survives");
        assert_eq!((field.start, field.end), (5, 6), "field spans the placeholder: {:?}", footer[0].marks);
        assert_eq!(field.value, MarkValue::Str(FIELD_PAGE_NUMBER.into()));

        // Idempotent across a second round-trip (block ids are volatile per import,
        // so compare on text + marks, not the full BlockView).
        let exported2 = OdtCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        OdtCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(norm_region(&d2.header()), norm_region(&d3.header()), "header stable across a second round-trip");
        assert_eq!(norm_region(&d2.footer()), norm_region(&d3.footer()), "footer stable across a second round-trip");
    }

    #[test]
    fn header_footer_preserve_existing_master_page_content() {
        // An imported styles.xml whose master-page references a page-layout must keep
        // that reference (+ any other master-page content) when the header/footer are
        // re-emitted on export.
        let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="pm1"><style:header><text:p>Old Header</text:p></style:header></style:master-page></office:master-styles></office:document-styles>"#;
        let base = minimal_odt_styled("", "<text:p>hi</text:p>");
        let mut c = OdtContainer::read(&base).unwrap();
        c.set("styles.xml", styles_xml.as_bytes().to_vec());
        let odt = c.write().unwrap();

        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        assert_eq!(d.header()[0].text, "Old Header", "existing header imported");
        // Replace the header text.
        let hid = d.header_blocks_in_order()[0].clone();
        d.splice_header_text(&hid, 0, 9, "New Header"); // replace "Old Header"

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let out = OdtContainer::read(&exported).unwrap();
        let styles_out = std::str::from_utf8(out.get("styles.xml").unwrap()).unwrap();
        assert!(styles_out.contains(r#"style:page-layout-name="pm1""#), "page-layout reference preserved: {styles_out}");
        assert!(styles_out.contains("New Header") && !styles_out.contains("Old Header"), "header updated: {styles_out}");
    }

    #[test]
    fn footnote_imports_from_inline_text_note() {
        use crate::document::MarkValue;
        use crate::schema::{FIELD_PLACEHOLDER, MARK_NOTE_REF};
        // A paragraph "see ref" with an inline footnote right after "see ": the body
        // gets a `note-ref` marker over a placeholder, and the note's body becomes a
        // note in the `notes` container.
        let body = concat!(
            r#"<text:p>see <text:note text:id="ftn1" text:note-class="footnote">"#,
            r#"<text:note-citation>1</text:note-citation>"#,
            r#"<text:note-body><text:p>the footnote text</text:p></text:note-body>"#,
            r#"</text:note>ref</text:p>"#,
        );
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        // Body paragraph carries the placeholder + a note-ref mark over it.
        let p = &snap.blocks[0];
        assert_eq!(p.text, format!("see {}ref", FIELD_PLACEHOLDER), "placeholder spliced inline: {:?}", p.text);
        let nref = p.marks.iter().find(|m| m.name == MARK_NOTE_REF).expect("note-ref mark present");
        assert_eq!((nref.start, nref.end), (4, 5), "marker covers exactly the placeholder: {:?}", p.marks);
        let nid = match &nref.value {
            MarkValue::Str(s) => s.clone(),
            other => panic!("note-ref value is the note id, got {other:?}"),
        };

        // The note exists with the right kind + body text.
        assert_eq!(snap.notes.len(), 1, "one note: {:?}", snap.notes);
        let note = &snap.notes[0];
        assert_eq!(note.id, nid, "marker references the note by id");
        assert_eq!(note.kind, "footnote");
        assert_eq!(note.blocks.len(), 1);
        assert_eq!(note.blocks[0].text, "the footnote text");
    }

    #[test]
    fn footnote_and_endnote_roundtrip_through_odt() {
        use crate::document::MarkValue;
        use crate::schema::MARK_NOTE_REF;
        // Build from scratch: a paragraph "see ref" with a footnote after "see ", and
        // a second paragraph with an endnote. Both must export as inline text:note
        // and re-import as the same markers + note bodies + kinds.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "see ref");
        let fid = d.insert_footnote(&p, 4); // after "see "
        let fb = d.insert_note_block(&fid, None, "paragraph").unwrap();
        d.splice_note_text(&fid, &fb, 0, 0, "a footnote");

        let p2 = d.insert_block(Some(&p), "paragraph");
        d.splice_text(&p2, 0, 0, "tail");
        let eid = d.insert_endnote(&p2, 4); // at the end
        let eb = d.insert_note_block(&eid, None, "paragraph").unwrap();
        d.splice_note_text(&eid, &eb, 0, 0, "an endnote");

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(content.contains(r#"text:note-class="footnote""#), "footnote emitted inline: {content}");
        assert!(content.contains(r#"text:note-class="endnote""#), "endnote emitted inline: {content}");
        assert!(content.contains("a footnote") && content.contains("an endnote"), "note bodies emitted: {content}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        // Body text + markers survive.
        assert_eq!(snap.blocks[0].text.chars().filter(|c| *c != '\u{FFFC}').collect::<String>(), "see ref");
        let nref = snap.blocks[0].marks.iter().find(|m| m.name == MARK_NOTE_REF).expect("footnote marker survives");
        let f_id = match &nref.value {
            MarkValue::Str(s) => s.clone(),
            _ => panic!("note-ref value"),
        };
        // Notes survive with kinds + bodies.
        assert_eq!(snap.notes.len(), 2, "both notes survive: {:?}", snap.notes);
        let foot = snap.notes.iter().find(|n| n.id == f_id).expect("footnote note present by id");
        assert_eq!(foot.kind, "footnote");
        assert_eq!(foot.blocks[0].text, "a footnote");
        let end = snap.notes.iter().find(|n| n.kind == "endnote").expect("endnote present");
        assert_eq!(end.blocks[0].text, "an endnote");

        // Idempotent across a second round-trip (ids volatile; compare on kind+text).
        let exported2 = OdtCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        OdtCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        let kinds_texts = |snap: &DocSnapshot| {
            let mut v: Vec<(String, String)> =
                snap.notes.iter().map(|n| (n.kind.clone(), n.blocks.iter().map(|b| b.text.clone()).collect())).collect();
            v.sort();
            v
        };
        assert_eq!(kinds_texts(&d2.materialize()), kinds_texts(&d3.materialize()), "notes stable across a second round-trip");
    }

    #[test]
    fn comment_imports_from_inline_annotation() {
        use crate::document::MarkValue;
        use crate::schema::MARK_COMMENT;
        // A paragraph "fix this word" with an annotation spanning "this word": the
        // annotation sits at the range start, an annotation-end at the range end.
        let body = concat!(
            r#"<text:p>fix "#,
            r#"<office:annotation office:name="c1"><dc:creator>Ada</dc:creator><dc:date>2021-01-02T03:04:05</dc:date><text:p>note: fix</text:p></office:annotation>"#,
            r#"this word"#,
            r#"<office:annotation-end office:name="c1"/>"#,
            r#"</text:p>"#,
        );
        let odt = minimal_odt(body);
        let mut d = Document::new();
        OdtCodec.import(&odt, &mut d.as_mut()).unwrap();
        let snap = d.materialize();

        // The body text excludes the annotation markup; a `comment` mark spans "this word".
        let p = &snap.blocks[0];
        assert_eq!(p.text, "fix this word", "annotation markup not in text: {:?}", p.text);
        let cm = p.marks.iter().find(|m| m.name == MARK_COMMENT).expect("comment mark present");
        assert_eq!((cm.start, cm.end), (4, 13), "marker spans 'this word': {:?}", p.marks);
        let cid = match &cm.value {
            MarkValue::Str(s) => s.clone(),
            other => panic!("comment value is the comment id, got {other:?}"),
        };
        // The comment exists with author + date + body.
        assert_eq!(snap.comments.len(), 1, "one comment: {:?}", snap.comments);
        let comment = &snap.comments[0];
        assert_eq!(comment.id, cid, "marker references the comment by id");
        assert_eq!(comment.author, "Ada");
        assert_eq!(comment.date, "2021-01-02T03:04:05");
        assert_eq!(comment.blocks[0].text, "note: fix");
    }

    #[test]
    fn comment_roundtrips_through_odt() {
        use crate::document::MarkValue;
        use crate::schema::MARK_COMMENT;
        // The headline task: a comment over a word must export as an inline
        // office:annotation(+ -end) and re-import with author/text + anchored range.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "please fix this word now");
        let cid = d.add_comment(&p, 11, 20, "Ada Lovelace", "note: fix"); // over "this word"
        let _ = cid;

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(content.contains("<office:annotation"), "annotation emitted: {content}");
        assert!(content.contains("<office:annotation-end"), "annotation-end emitted (ranged): {content}");
        assert!(content.contains("<dc:creator>Ada Lovelace</dc:creator>"), "author emitted: {content}");
        assert!(content.contains("note: fix"), "comment body emitted: {content}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        // Body text intact; the comment mark anchors "this word".
        assert_eq!(snap.blocks[0].text, "please fix this word now");
        let cm = snap.blocks[0].marks.iter().find(|m| m.name == MARK_COMMENT).expect("comment marker survives");
        assert_eq!((cm.start, cm.end), (11, 20), "anchored range survives: {:?}", snap.blocks[0].marks);
        let new_id = match &cm.value {
            MarkValue::Str(s) => s.clone(),
            _ => panic!("comment value"),
        };
        // The comment survives with author + body.
        assert_eq!(snap.comments.len(), 1, "comment survives: {:?}", snap.comments);
        let cmt = snap.comments.iter().find(|c| c.id == new_id).expect("comment present by id");
        assert_eq!(cmt.author, "Ada Lovelace");
        assert_eq!(cmt.blocks[0].text, "note: fix");
        assert_eq!(cmt.range, Some((snap.blocks[0].id.clone(), 11, 20)));

        // Idempotent across a second round-trip (ids volatile; compare author+text+span).
        let exported2 = OdtCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        OdtCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        let authors_texts = |snap: &DocSnapshot| {
            let mut v: Vec<(String, String, Option<(usize, usize)>)> = snap
                .comments
                .iter()
                .map(|c| {
                    (
                        c.author.clone(),
                        c.blocks.iter().map(|b| b.text.clone()).collect(),
                        c.range.as_ref().map(|(_, s, e)| (*s, *e)),
                    )
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            authors_texts(&d2.materialize()),
            authors_texts(&d3.materialize()),
            "comments stable across a second round-trip"
        );
    }

    #[test]
    fn tracked_changes_roundtrip_through_odt() {
        use crate::document::MarkValue;
        use crate::schema::{MARK_DEL, MARK_INS};
        // A tracked insertion + a tracked deletion (with author) must export as a
        // text:tracked-changes block + inline change-start/change-end markers, and
        // re-import with kind/author + anchored ranges intact.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        // Insert " there" after "hello" (idx 5).
        let ins = d.track_insert(&p, 5, " there", "Ada", "2021-01-02T00:00:00");
        // Delete "world" (now chars 12..17 in "hello there world").
        let del = d.track_delete(&p, 12, 17, "Bob", "2021-03-04T00:00:00");
        let _ = (ins, del);

        let exported = OdtCodec.export(d.as_ref()).unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(content.contains("<text:tracked-changes>"), "tracked-changes block emitted: {content}");
        assert!(content.contains("<text:insertion>"), "insertion region emitted: {content}");
        assert!(content.contains("<text:deletion>"), "deletion region emitted: {content}");
        assert!(content.contains("<text:change-start"), "change-start marker emitted: {content}");
        assert!(content.contains("<text:change-end"), "change-end marker emitted: {content}");
        assert!(content.contains("<dc:creator>Ada</dc:creator>"), "ins author emitted: {content}");
        assert!(content.contains("<dc:creator>Bob</dc:creator>"), "del author emitted: {content}");

        let mut d2 = Document::new();
        OdtCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        // Body text retains both the insertion and the (struck) deletion.
        assert_eq!(snap.blocks[0].text, "hello there world", "ins kept, del retained: {:?}", snap.blocks[0].text);
        let im = snap.blocks[0].marks.iter().find(|m| m.name == MARK_INS).expect("ins mark survives");
        assert_eq!((im.start, im.end), (5, 11), "ins range survives: {:?}", snap.blocks[0].marks);
        let dm = snap.blocks[0].marks.iter().find(|m| m.name == MARK_DEL).expect("del mark survives");
        assert_eq!((dm.start, dm.end), (12, 17), "del range survives: {:?}", snap.blocks[0].marks);
        let ins_id = match &im.value {
            MarkValue::Str(s) => s.clone(),
            _ => panic!("ins value"),
        };
        let del_id = match &dm.value {
            MarkValue::Str(s) => s.clone(),
            _ => panic!("del value"),
        };
        assert_eq!(snap.changes.len(), 2, "both changes survive: {:?}", snap.changes);
        let ic = snap.changes.iter().find(|c| c.id == ins_id).expect("ins change present");
        assert_eq!(ic.kind, crate::snapshot::ChangeKind::Ins);
        assert_eq!(ic.author, "Ada");
        assert_eq!(ic.range, Some((snap.blocks[0].id.clone(), 5, 11)));
        let dc = snap.changes.iter().find(|c| c.id == del_id).expect("del change present");
        assert_eq!(dc.kind, crate::snapshot::ChangeKind::Del);
        assert_eq!(dc.author, "Bob");
        assert_eq!(dc.range, Some((snap.blocks[0].id.clone(), 12, 17)));

        // Accept on the imported document: insertion clears its mark (text kept),
        // deletion removes its struck text.
        d2.accept_change(&ins_id);
        d2.accept_change(&del_id);
        let after = d2.materialize();
        assert_eq!(after.blocks[0].text, "hello there ", "accept-ins keeps text; accept-del removes 'world'");
        assert!(after.changes.is_empty(), "both changes resolved");
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
