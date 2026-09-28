//! Export the canonical model back to a `.odt`: generate a fresh minimal ODF body from
//! the model, then splice it into the original container's preserved parts (styles.xml,
//! images, meta.xml, manifest, …) so unmodeled content survives verbatim.

use super::container::OdtContainer;
use crate::format::CodecError;
use crate::preserve::MARK_ODT_XML;
use crate::resolved::Resolved;
use std::collections::{BTreeMap, BTreeSet};
use tilo_content::ContentSource;
use tilo_doc::document::MarkSpan;
use tilo_doc::export::InlinePlan;
use tilo_doc::model::DocRef;
use tilo_doc::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME,
    STYLE_SIZE, STYLE_SPACE_AFTER, STYLE_SPACE_BEFORE,
};
use tilo_doc::snapshot::{AttrVal, DocSnapshot};
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

const MIMETYPE: &[u8] = b"application/vnd.oasis.opendocument.text";
/// A minimal `styles.xml` skeleton (automatic-styles + master-styles wired to a `pm1`
/// page-layout), used as the splice target when a multi-column document had no styles.xml.
const MINIMAL_STYLES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties/></style:page-layout></office:automatic-styles><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="pm1"/></office:master-styles></office:document-styles>"#;
const MANIFEST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0">
<manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>
<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>"#;

#[allow(
    clippy::needless_pass_by_value,
    reason = "matches FormatProvider::export's by-value DocRef; its one call site (odt/mod.rs) is outside this file-only pass's scope"
)]
pub(crate) fn export(doc: DocRef, content: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
    let snapshot = doc.materialize();
    let snap = &Resolved::new(&snapshot, content);
    let preserved = tilo_doc::model::preserved_parts(doc.doc, doc.c, content);
    // Named styles already defined in a preserved `styles.xml` must not be re-emitted into
    // `content.xml` (that would duplicate the definition) — only referenced.
    let existing_style_names = preserved
        .iter()
        .find(|(k, _)| k == "odt/part/styles.xml")
        .map(|(_, b)| style_names_in(b))
        .unwrap_or_default();
    let head = ContentHead::preserved(&doc);
    let content = build_content_xml(snap, &existing_style_names, &head);

    let mut c = OdtContainer { parts: Vec::new() };
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
    splice_styles_xml(&doc, &mut c, original_styles.as_deref(), &mut had_styles);
    // Document properties → `meta.xml`: regenerate from the canonical properties, merging in
    // any unmodeled `office:meta` children from the preserved original so they survive.
    if let Some(meta_xml) = super::meta::export_meta(&doc, original_meta.as_deref()) {
        let is_new = !had_meta;
        c.set("meta.xml", meta_xml);
        if is_new {
            had_meta = true;
        }
    }
    // Fall back to generated mimetype/manifest when the source had none (e.g. a doc created from scratch).
    if !had_mimetype {
        c.set("mimetype", MIMETYPE.to_vec());
    }
    if !had_manifest {
        c.set("META-INF/manifest.xml", MANIFEST.to_vec());
    }

    let pic_entries = write_image_blobs(snap, &mut c, had_meta, had_styles);
    if !pic_entries.is_empty() {
        let manifest = c
            .get("META-INF/manifest.xml")
            .map_or_else(|| MANIFEST.to_vec(), <[u8]>::to_vec);
        c.set(
            "META-INF/manifest.xml",
            inject_manifest_entries(&manifest, &pic_entries),
        );
    }

    c.set("content.xml", content.into_bytes());
    c.write()
}

/// Splice header/footer, multi-column layout, and default-language into `styles.xml`
/// (synthesising a minimal one if none was preserved). Sets `had_styles` the first time
/// anything is actually written, so the caller knows whether to add a manifest entry for it.
fn splice_styles_xml(
    doc: &DocRef,
    c: &mut OdtContainer,
    original_styles: Option<&[u8]>,
    had_styles: &mut bool,
) {
    // Page header/footer → `styles.xml` master-page: splice regenerated header/footer into a
    // copy of `styles.xml` (or synthesise one). No-op when the document has neither.
    if let Some(styles_xml) = super::headers::export_master_headers(doc, original_styles) {
        let is_new = !*had_styles;
        c.set("styles.xml", styles_xml);
        if is_new {
            *had_styles = true;
        }
    }
    // Multi-column layout → `styles.xml` page-layout: splice `style:columns` into the first
    // page-layout properties, preserving the rest. No-op for a single-column document.
    {
        let base = c
            .get("styles.xml")
            .map_or_else(|| MINIMAL_STYLES.to_vec(), <[u8]>::to_vec);
        if let Some(styles_xml) = super::columns::export_page_layout_columns(doc, &base) {
            let is_new = !*had_styles;
            c.set("styles.xml", styles_xml);
            if is_new {
                *had_styles = true;
            }
        }
    }
    // Document default language → `styles.xml` paragraph default-style: splice the BCP-47 tag
    // (split into `fo:language`/`fo:country`). No-op when the document declares no default language.
    {
        let base = c
            .get("styles.xml")
            .map_or_else(|| MINIMAL_STYLES.to_vec(), <[u8]>::to_vec);
        if let Some(styles_xml) = super::language::export_default_language(doc, &base) {
            let is_new = !*had_styles;
            c.set("styles.xml", styles_xml);
            if is_new {
                *had_styles = true;
            }
        }
    }
}

/// Write each `image` block's blob into the zip as `Pictures/<hash>.<ext>` and return the
/// manifest entries it needs, seeded with meta.xml/styles.xml when those were (re)written.
fn write_image_blobs(
    snap: &Resolved<'_>,
    c: &mut OdtContainer,
    had_meta: bool,
    had_styles: bool,
) -> Vec<(String, &'static str)> {
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
        if let Some(AttrVal::Str(hash)) = b.attrs.get("blob")
            && let Some(bytes) = snap.image(hash)
        {
            let (ext, media) = image_ext_and_media(bytes);
            let path = format!("Pictures/{hash}.{ext}");
            c.set(&path, bytes.to_vec());
            if !pic_entries.iter().any(|(p, _)| p == &path) {
                pic_entries.push((path, media));
            }
        }
    }
    pic_entries
}

/// Detect an image's file extension + ODF media type from its magic bytes; unknown payloads fall back to a generic binary stream.
fn image_ext_and_media(bytes: &[u8]) -> (&'static str, &'static str) {
    tilo_doc::export::detect_image_media(bytes)
        .filter(|media| matches!(media.extension, "png" | "jpg" | "gif"))
        .map_or(("bin", "application/octet-stream"), |media| {
            (media.extension, media.mime)
        })
}

/// The `Pictures/<hash>.<ext>` href an image block's blob is written under.
fn picture_href(hash: &str, bytes: &[u8]) -> String {
    let (ext, _) = image_ext_and_media(bytes);
    format!("Pictures/{hash}.{ext}")
}

/// Insert `manifest:file-entry` rows for `entries` not already present, just before the closing `</manifest:manifest>`.
fn inject_manifest_entries(manifest: &[u8], entries: &[(String, &'static str)]) -> Vec<u8> {
    let s = String::from_utf8_lossy(manifest);
    let mut inserts = String::new();
    for (path, media) in entries {
        if s.contains(&format!(r#"manifest:full-path="{path}""#)) {
            continue;
        }
        let _ = write!(
            inserts,
            r#"<manifest:file-entry manifest:full-path="{path}" manifest:media-type="{media}"/>"#
        );
    }
    if inserts.is_empty() {
        return manifest.to_vec();
    }
    match s
        .rfind("</manifest:manifest>")
        .and_then(|pos| s.get(..pos).zip(s.get(pos..)))
    {
        Some((before, after)) => {
            let mut out = String::with_capacity(s.len().saturating_add(inserts.len()));
            out.push_str(before);
            out.push_str(&inserts);
            out.push_str(after);
            out.into_bytes()
        }
        None => manifest.to_vec(),
    }
}

/// Codec-authored automatic *text* style for a mark: `(style name, properties)`. Boolean marks map to fixed styles; valued font marks get a per-value style.
fn mark_style(m: &MarkSpan) -> Option<(String, String)> {
    match m.name.as_str() {
        "strong" => Some(("qdoc-strong".into(), r#"fo:font-weight="bold""#.into())),
        "em" => Some(("qdoc-em".into(), r#"fo:font-style="italic""#.into())),
        "underline" => Some((
            "qdoc-underline".into(),
            r#"style:text-underline-style="solid""#.into(),
        )),
        "strike" => Some((
            "qdoc-strike".into(),
            r#"style:text-line-through-style="solid""#.into(),
        )),
        "fontSize" => m
            .value
            .as_int()
            .map(|n| (format!("qdoc-fs-{n}"), format!(r#"fo:font-size="{n}pt""#))),
        "fontFamily" => m.value.as_str().map(|f| {
            (
                format!("qdoc-ff-{f}"),
                format!(r#"style:font-name="{}""#, font_name(f)),
            )
        }),
        "color" => m.value.as_str().map(|c| {
            (
                format!("qdoc-color-{}", color_token(c)),
                format!(r#"fo:color="{c}""#),
            )
        }),
        "highlight" => m.value.as_str().map(|c| {
            (
                format!("qdoc-hl-{}", color_token(c)),
                format!(r#"fo:background-color="{c}""#),
            )
        }),
        // Vertical alignment: ODF `style:text-position` (keyword + 58% font height, matching what LibreOffice writes for menu super/subscript).
        "super" => Some((
            "qdoc-super".into(),
            r#"style:text-position="super 58%""#.into(),
        )),
        "sub" => Some(("qdoc-sub".into(), r#"style:text-position="sub 58%""#.into())),
        // Per-run language override: split BCP-47 `de-DE` into ODF `fo:language="de" fo:country="DE"` text-properties.
        "lang" => m
            .value
            .as_str()
            .filter(|c| !c.is_empty())
            .map(|c| (format!("qdoc-lang-{}", lang_token(c)), lang_text_props(c))),
        _ => None,
    }
}

/// A style-name-safe token for a colour value (`#rrggbb` → `rrggbb`); keeps the generated automatic-style name a valid XML name.
fn color_token(c: &str) -> String {
    c.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// A style-name-safe token for a BCP-47 tag (`de-DE` → `deDE`); keeps the generated automatic-style name a valid XML name.
fn lang_token(c: &str) -> String {
    c.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// The ODF `style:text-properties` attribute string for a BCP-47 tag: `xx-YY` splits into
/// `fo:language="xx" fo:country="YY"` (`xx` alone emits just `fo:language`).
fn lang_text_props(code: &str) -> String {
    match code.split_once('-') {
        Some((lang, country)) if !country.is_empty() => {
            format!(
                r#"fo:language="{}" fo:country="{}""#,
                escape_attr(lang),
                escape_attr(country)
            )
        }
        _ => format!(r#"fo:language="{}""#, escape_attr(code)),
    }
}

type NoteIndex<'a> = std::collections::BTreeMap<String, &'a tilo_doc::snapshot::NoteView>;

fn note_index(snap: &DocSnapshot) -> NoteIndex<'_> {
    snap.notes.iter().map(|n| (n.id.clone(), n)).collect()
}

/// A comment-id → `&CommentView` lookup for emitting `office:annotation`(+`-end`) at a commented range's boundaries on export.
type CommentIndex<'a> = std::collections::BTreeMap<String, &'a tilo_doc::snapshot::CommentView>;

fn comment_index(snap: &DocSnapshot) -> CommentIndex<'_> {
    snap.comments.iter().map(|c| (c.id.clone(), c)).collect()
}

/// What inline rendering reads and accumulates across the whole body.
struct InlineCtx<'a> {
    /// Generated span styles referenced so far (see [`span_styles`]).
    used: BTreeMap<String, String>,
    notes: NoteIndex<'a>,
    /// Inline notes emitted so far, numbering them in document order.
    note_ctr: usize,
    comments: CommentIndex<'a>,
}

/// Render a block's text with `text:a` hyperlinks (outer) and `text:span` style wrappers
/// (inner), closing and re-opening each whenever it changes so output stays well-nested even
/// for overlapping marks; a `note-ref` placeholder emits the referenced inline `text:note`.
fn render_inline(text: &str, marks: &[MarkSpan], ctx: &mut InlineCtx<'_>) -> String {
    let plan = InlinePlan::new(text, marks);
    let text_len = text.chars().count();
    let mut out = String::new();
    // A leading space would be collapsed away; see `push_odf_text`.
    let mut after_space = true;
    let mut open: Vec<String> = Vec::new();
    let mut cur_link: Option<&str> = None;
    let close_spans = |out: &mut String, open: &mut Vec<String>| {
        for _ in 0..open.len() {
            out.push_str("</text:span>");
        }
        open.clear();
    };
    let close_inline = |out: &mut String, open: &mut Vec<String>, cur_link: &mut Option<&str>| {
        close_spans(out, open);
        if cur_link.take().is_some() {
            out.push_str("</text:a>");
        }
    };

    for point in plan.points() {
        let boundary = &point.boundary;
        if !boundary.is_empty() {
            close_inline(&mut out, &mut open, &mut cur_link);

            // ODF boundary elements must be siblings. End each range before
            // opening an adjacent one; the shared plan supplies the stable,
            // format-neutral ordering.
            for id in &boundary.comments.ends {
                if let Some(comment) = ctx.comments.get(*id) {
                    out.push_str(&super::comments::render_annotation_end(comment));
                }
            }
            for id in &boundary.comments.starts {
                if let Some(comment) = ctx.comments.get(*id) {
                    out.push_str(&super::comments::render_annotation_start(comment));
                }
            }
            for name in &boundary.bookmarks.ends {
                let _ = write!(
                    out,
                    r#"<text:bookmark-end text:name="{}"/>"#,
                    escape_attr(name)
                );
            }
            for name in &boundary.bookmarks.starts {
                let _ = write!(
                    out,
                    r#"<text:bookmark-start text:name="{}"/>"#,
                    escape_attr(name)
                );
            }
            for id in &boundary.changes.ends {
                out.push_str(&super::changes::render_change_end(id));
            }
            for id in &boundary.changes.starts {
                out.push_str(&super::changes::render_change_start(id));
            }
        }

        let Some(run) = point.run.as_ref() else {
            debug_assert_eq!(point.offset, text_len);
            continue;
        };
        let link = run.link;
        if link != cur_link {
            // A link is the outer element, so close any open spans first, then the old link, then open the new link (spans re-open below).
            close_spans(&mut out, &mut open);
            if cur_link.is_some() {
                out.push_str("</text:a>");
            }
            if let Some(href) = link {
                let _ = write!(out, r#"<text:a xlink:href="{}">"#, escape_attr(href));
            }
            cur_link = link;
        }
        let want = span_styles(run, &mut ctx.used);
        if want != open {
            close_spans(&mut out, &mut open);
            for name in &want {
                let _ = write!(out, r#"<text:span text:style-name="{name}">"#);
                open.push(name.clone());
            }
        }
        if let Some(note_id) = run.note_reference {
            if let Some(note) = ctx.notes.get(note_id) {
                // A per-document note counter; bounded by how many notes
                // one document has, nowhere near `usize::MAX` (or the
                // note-counter's actual integer type's max).
                ctx.note_ctr = ctx.note_ctr.saturating_add(1);
                out.push_str(&super::notes::render_inline_note(note, ctx.note_ctr));
            }
            after_space = false;
            continue;
        }
        push_run_text(&mut out, run, &mut after_space);
    }
    close_inline(&mut out, &mut open, &mut cur_link);
    out
}

/// The text styles a run is wrapped in, outermost first: a kept text style, then the
/// generated ones stating the modeled properties (registered in `used`).
fn span_styles(
    run: &tilo_doc::export::InlineRun<'_>,
    used: &mut std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    let mut want: Vec<String> = run
        .marks
        .iter()
        .filter_map(|m| {
            mark_style(m).map(|(name, props)| {
                used.insert(name.clone(), props);
                name
            })
        })
        .collect();
    want.sort();
    want.dedup();
    if let Some(kept) = run
        .marks
        .iter()
        .find(|m| m.name == super::content::MARK_ODT_SPAN)
        .and_then(|m| m.value.as_str())
    {
        want.insert(0, escape_attr(kept));
    }
    want
}

/// Append a run's content: the unmodeled inline element kept verbatim that it shows, while it
/// still shows it unedited (see `preserve::run_content`), then its text.
fn push_run_text(out: &mut String, run: &tilo_doc::export::InlineRun<'_>, after_space: &mut bool) {
    let content = crate::preserve::run_content(run.text, &run.marks, MARK_ODT_XML);
    if let Some(xml) = content.kept {
        out.push_str(xml);
        *after_space = false;
    }
    push_odf_text(out, &content.text, after_space);
}

/// Append paragraph text, writing what ODF whitespace handling would otherwise collapse as
/// elements: a tab as `text:tab`, a line break as `text:line-break`, and a space at the start
/// or after another space as `text:s`. `after_space` carries over between runs of a paragraph.
fn push_odf_text(out: &mut String, text: &str, after_space: &mut bool) {
    for c in text.chars() {
        match c {
            '\t' => {
                out.push_str("<text:tab/>");
                *after_space = true;
            }
            '\n' => {
                out.push_str("<text:line-break/>");
                *after_space = true;
            }
            ' ' if *after_space => out.push_str("<text:s/>"),
            ' ' => {
                out.push(' ');
                *after_space = true;
            }
            _ => {
                push_text(out, c);
                *after_space = false;
            }
        }
    }
}

fn font_name(generic: &str) -> &'static str {
    match generic {
        "serif" => "Liberation Serif",
        "mono" => "Liberation Mono",
        _ => "Liberation Sans",
    }
}

/// Paragraph-properties for a block's alignment, if non-default (font is a per-run mark, handled by `render_inline`).
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
            let _ = write!(pp, r#" fo:text-align="{v}""#);
        }
    }
    // Line spacing → `fo:line-height` percent (1.5 → "150%").
    if let Some(AttrVal::Float(ls)) = b.attrs.get("line_spacing")
        && *ls > 0.0
    {
        let _ = write!(pp, r#" fo:line-height="{}%""#, fmt_num(ls * 100.0));
    }
    if pp.is_empty() {
        None
    } else {
        Some((pp, String::new()))
    }
}

fn image_anchor(b: &BlockView) -> &str {
    match b.attrs.get("anchor") {
        Some(AttrVal::Str(s)) => s.as_str(),
        _ => "inline",
    }
}

fn image_wrap(b: &BlockView) -> &str {
    match b.attrs.get("wrap") {
        Some(AttrVal::Str(s)) => s.as_str(),
        _ => "none",
    }
}

/// Build the `draw:frame` XML for an `image` block, or `None` if its blob is absent; the
/// `xlink:href` matches the `Pictures/<hash>.<ext>` part `export()` writes for the same blob.
fn image_frame_xml(
    b: &BlockView,
    snap: &Resolved<'_>,
    graphic_styles: &mut Vec<(String, String)>,
) -> Option<String> {
    let Some(AttrVal::Str(hash)) = b.attrs.get("blob") else {
        return None;
    };
    let bytes = snap.image(hash)?;
    let href = picture_href(hash, bytes);
    let anchor = image_anchor(b);
    let wrap = image_wrap(b);
    let mut frame = String::from("<draw:frame");
    let _ = write!(frame, r#" draw:name="{}""#, escape_attr(&b.id));
    let anchor_type = match anchor {
        "inline" => "as-char",
        "page" => "page",
        _ => "paragraph",
    };
    let _ = write!(frame, r#" text:anchor-type="{anchor_type}""#);
    // A graphic automatic style carries the wrap; only emit it when meaningful (a floating frame, since `as-char` ignores wrap in ODF anyway).
    if anchor != "inline" {
        let gname = format!("Grqdoc{}", graphic_styles.len().saturating_add(1));
        graphic_styles.push((
            gname.clone(),
            super::styles::model_wrap_to_odf(wrap).to_string(),
        ));
        let _ = write!(frame, r#" draw:style-name="{gname}""#);
    }
    if let Some(AttrVal::Int(w)) = b.attrs.get("width")
        && *w > 0
    {
        let _ = write!(frame, r#" svg:width="{w}px""#);
    }
    if let Some(AttrVal::Int(h)) = b.attrs.get("height")
        && *h > 0
    {
        let _ = write!(frame, r#" svg:height="{h}px""#);
    }
    frame.push('>');
    let _ = write!(
        frame,
        r#"<draw:image xlink:href="{}"/>"#,
        escape_attr(&href)
    );
    if let Some(AttrVal::Str(alt)) = b.attrs.get("alt")
        && !alt.is_empty()
    {
        let _ = write!(frame, "<svg:title>{}</svg:title>", escape_text(alt));
    }
    frame.push_str("</draw:frame>");
    Some(frame)
}

fn snap_list_level(b: &BlockView) -> i64 {
    match b.attrs.get("level") {
        Some(AttrVal::Int(n)) if *n >= 0 => *n,
        _ => 0,
    }
}

fn snap_list_ordered(b: &BlockView) -> bool {
    matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)))
}

fn snap_num_format(b: &BlockView) -> &str {
    match b.attrs.get("num_format") {
        Some(AttrVal::Str(s)) => s.as_str(),
        _ => "decimal",
    }
}

fn snap_num_start(b: &BlockView) -> i64 {
    match b.attrs.get("num_start") {
        Some(AttrVal::Int(n)) => *n,
        _ => 1,
    }
}

fn snap_num_restart(b: &BlockView) -> bool {
    matches!(b.attrs.get("num_restart"), Some(AttrVal::Bool(true)))
}

/// Map the model's canonical numbering format onto an ODF `style:num-format` value (inverse of `styles::map_num_format`).
fn odf_num_format(fmt: &str) -> &'static str {
    match fmt {
        "lower-roman" => "i",
        "upper-roman" => "I",
        "lower-alpha" => "a",
        "upper-alpha" => "A",
        _ => "1",
    }
}

/// A `text:list-style` definition reconstructing a level-0 numbering (format + start) so an
/// ordered list re-imports with its numbering intact; `name` is the generated automatic-style name.
/// A `text:list-style` definition for a list of `key`'s kind: numbered in the model's format
/// from its start, or bulleted. Every level is defined, since a nested list without a style of
/// its own continues its parent's at the deeper level.
fn list_style_def_xml(name: &str, key: &ListKey) -> String {
    let mut levels = String::new();
    for level in 1..=10 {
        if key.ordered {
            let nf = odf_num_format(&key.format);
            let sv = if key.start == 1 || level > 1 {
                String::new()
            } else {
                format!(r#" text:start-value="{}""#, key.start)
            };
            let _ = write!(
                levels,
                r#"<text:list-level-style-number text:level="{level}" style:num-suffix="." style:num-format="{nf}"{sv}/>"#
            );
        } else {
            let _ = write!(
                levels,
                r#"<text:list-level-style-bullet text:level="{level}" text:bullet-char="•"/>"#
            );
        }
    }
    format!(r#"<text:list-style style:name="{name}">{levels}</text:list-style>"#)
}

/// What a list's style encodes: numbered (in a format, from a start) or bulleted.
#[derive(Clone, PartialEq, Eq)]
struct ListKey {
    ordered: bool,
    format: String,
    start: i64,
}

impl ListKey {
    fn of(li: &BlockView) -> Self {
        let ordered = snap_list_ordered(li);
        Self {
            ordered,
            format: if ordered {
                snap_num_format(li).to_string()
            } else {
                String::new()
            },
            start: if ordered { snap_num_start(li) } else { 1 },
        }
    }
}

/// A registered reconstructed list-style: its generated name + the key it encodes + its full XML.
struct ListStyle {
    name: String,
    key: ListKey,
    xml: String,
}

/// The opening `<text:list …>` tag for the list `li` lives in, referencing a reconstructed
/// `text:list-style` (deduped on its key), plus `text:start-value` (a restart) or
/// `text:continue-numbering`. Without a style a list re-imports, and renders, unnumbered.
fn open_list_tag(li: &BlockView, head: &ContentHead, list_styles: &mut Vec<ListStyle>) -> String {
    let key = ListKey::of(li);
    let start = key.start;
    let name = if let Some(kept) = head.reused_list_style(li, &key) {
        kept.to_string()
    } else if let Some(s) = list_styles.iter().find(|s| s.key == key) {
        s.name.clone()
    } else {
        let name = format!("Lqdoc{}", list_styles.len().saturating_add(1));
        let xml = list_style_def_xml(&name, &key);
        list_styles.push(ListStyle {
            name: name.clone(),
            key,
            xml,
        });
        name
    };
    let mut tag = format!(r#"<text:list text:style-name="{name}""#);
    if start != 1 && snap_num_restart(li) {
        let _ = write!(tag, r#" text:start-value="{start}""#);
    } else if start != 1 {
        // A non-default start that continues: mark it continuing so the importer doesn't treat it as a fresh restart.
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
        Some(AttrVal::Int(n)) if *n >= 0 => usize::try_from(*n).unwrap_or(0),
        _ => 0,
    };
    (g("row"), g("col"))
}

/// A resolved paragraph automatic style: `(parent style name, paragraph-properties XML,
/// text-properties XML)`.
type ParaStyleEntry = (Option<String>, String, String);

/// Get-or-create a paragraph automatic style for `(parent, paragraph-props, text-props)`, returning its `P{n}` name. Dedups identical styles.
fn register_auto(styles: &mut Vec<ParaStyleEntry>, key: ParaStyleEntry) -> String {
    if let Some(i) = styles.iter().position(|p| p == &key) {
        format!("Pqdoc{}", i.saturating_add(1))
    } else {
        styles.push(key);
        format!("Pqdoc{}", styles.len())
    }
}

/// The `text:style-name="…"` attribute for a block, resolving an explicit named style and/or
/// an alignment override; registers any referenced named/derived automatic style as a side effect.
fn resolve_para_style(
    b: &BlockView,
    head: &ContentHead,
    styles: &BTreeMap<String, BTreeMap<String, AttrVal>>,
    auto: &mut Vec<ParaStyleEntry>,
    named_refs: &mut BTreeSet<String>,
) -> String {
    // The imported automatic style, while the model still agrees with it.
    if let Some(style) = head.reused_para_style(b) {
        return format!(r#" text:style-name="{}""#, escape_attr(style));
    }
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

fn fmt_num(f: f64) -> String {
    if f.fract() == 0.0 {
        // Floats have no checked int conversion; clamp to a range no real document
        // measurement approaches before the cast.
        let clamped = f.clamp(-1e15, 1e15);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped to ±1e15 above, far inside i64 range"
        )]
        let n = clamped as i64;
        format!("{n}")
    } else {
        let s = format!("{f:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Style names already declared in a preserved `styles.xml`, so the exporter can reference rather than redefine them.
fn style_names_in(xml: &[u8]) -> BTreeSet<String> {
    let s = String::from_utf8_lossy(xml);
    let mut out = BTreeSet::new();
    let needle = "style:name=\"";
    let mut rest = s.as_ref();
    while let Some(p) = rest.find(needle) {
        let Some(after_needle) = p
            .checked_add(needle.len())
            .and_then(|start| rest.get(start..))
        else {
            break;
        };
        rest = after_needle;
        let Some(end) = rest.find('"') else {
            break;
        };
        if let Some(name) = rest.get(..end) {
            out.insert(name.to_string());
        }
        let Some(after_quote) = end.checked_add(1).and_then(|start| rest.get(start..)) else {
            break;
        };
        rest = after_quote;
    }
    out
}

/// An ODF `style:style` definition (family paragraph) for a CRDT styles-container style, so `LibreOffice` renders the document's named styles.
fn named_style_def_xml(id: &str, props: &BTreeMap<String, AttrVal>) -> String {
    let name = super::styles::encode_style_name(id);
    let getf = |k: &str| match props.get(k) {
        Some(AttrVal::Float(f)) => Some(*f),
        #[allow(
            clippy::cast_precision_loss,
            reason = "style properties (font size, spacing, indent) are always small; i64 has no checked f64 conversion"
        )]
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
        let _ = write!(tp, r#" fo:font-size="{}pt""#, fmt_num(sz));
    }
    if let Some(f) = gets(STYLE_FONT) {
        let _ = write!(tp, r#" style:font-name="{}""#, font_name(f));
    }
    if let Some(c) = gets(STYLE_COLOR) {
        let _ = write!(tp, r#" fo:color="{}""#, escape_attr(c));
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
            let _ = write!(pp, r#" fo:text-align="{v}""#);
        }
    }
    if let Some(v) = getf(STYLE_SPACE_BEFORE)
        && v != 0.0
    {
        let _ = write!(pp, r#" fo:margin-top="{}px""#, fmt_num(v));
    }
    if let Some(v) = getf(STYLE_SPACE_AFTER)
        && v != 0.0
    {
        let _ = write!(pp, r#" fo:margin-bottom="{}px""#, fmt_num(v));
    }
    if let Some(v) = getf(STYLE_INDENT)
        && v != 0.0
    {
        let _ = write!(pp, r#" fo:margin-left="{}px""#, fmt_num(v));
    }
    let display = gets(STYLE_NAME)
        .map(|n| format!(r#" style:display-name="{}""#, escape_attr(n)))
        .unwrap_or_default();
    format!(
        r#"<style:style style:name="{name}" style:family="paragraph"{display}><style:paragraph-properties{pp}/><style:text-properties{tp}/></style:style>"#
    )
}

use crate::xml_escape::{escape_attr, escape_text, push_text};
use tilo_doc::snapshot::BlockView;

/// The imported `content.xml`'s root start tag, prelude and automatic styles (see
/// `content::content_head`), written back around the regenerated body. Generated automatic
/// style names carry a `qdoc` infix so they never collide with the carried-through ones.
#[derive(Default)]
struct ContentHead {
    root: Option<String>,
    prelude: String,
    automatic_styles: String,
    /// The carried automatic styles and the preserved `styles.xml`, parsed as import does, to
    /// tell whether a kept style reference still agrees with the model.
    registry: super::styles::StyleRegistry,
    automatic_names: BTreeSet<String>,
}

impl ContentHead {
    fn preserved(doc: &DocRef) -> Self {
        let get = |key: &str| {
            tilo_doc::model::preserved_part(doc.doc, doc.c, key, &tilo_content::NoContent)
                .and_then(|b| String::from_utf8(b).ok())
        };
        let automatic_styles = get(super::CONTENT_AUTOMATIC_STYLES_KEY).unwrap_or_default();
        let mut registry = super::styles::StyleRegistry::default();
        if let Some(styles) = tilo_doc::model::preserved_part(
            doc.doc,
            doc.c,
            "odt/part/styles.xml",
            &tilo_content::NoContent,
        ) {
            super::styles::parse_styles(&styles, &mut registry, true);
        }
        super::styles::parse_styles(automatic_styles.as_bytes(), &mut registry, false);
        Self {
            root: get(super::CONTENT_ROOT_KEY),
            prelude: get(super::CONTENT_PRELUDE_KEY).unwrap_or_default(),
            automatic_names: style_names_in(automatic_styles.as_bytes()),
            automatic_styles,
            registry,
        }
    }

    /// The imported style of the list `li` is in, while it still numbers `li` the way the
    /// model says (`key`).
    fn reused_list_style<'b>(&self, li: &'b BlockView, key: &ListKey) -> Option<&'b str> {
        let Some(AttrVal::Str(style)) = li.attrs.get(super::content::ODT_LIST_STYLE) else {
            return None;
        };
        let ordered = self.registry.list_ordered.get(style.as_str()).copied()?;
        let numbering = self.registry.list_numbering.get(style.as_str());
        let format = numbering.and_then(|n| n.num_format).unwrap_or("decimal");
        let start = numbering.and_then(|n| n.num_start).unwrap_or(1);
        let agrees =
            ordered == key.ordered && (!ordered || (format == key.format && start == key.start));
        agrees.then_some(style.as_str())
    }

    /// The imported automatic paragraph style of `b`, while the block's modeled paragraph
    /// attributes (no named style, alignment, line spacing) still agree with it.
    fn reused_para_style<'b>(&self, b: &'b BlockView) -> Option<&'b str> {
        let Some(AttrVal::Str(style)) = b.attrs.get(super::content::ODT_STYLE) else {
            return None;
        };
        let align = match b.attrs.get("align") {
            Some(AttrVal::Str(a)) => Some(a.clone()),
            _ => None,
        };
        let line = match b.attrs.get("line_spacing") {
            Some(AttrVal::Float(f)) => Some(*f),
            _ => None,
        };
        let agrees = self.automatic_names.contains(style)
            && !b.attrs.contains_key("style")
            && self.registry.para_align(style) == align
            && self.registry.para_line_height(style) == line;
        agrees.then_some(style.as_str())
    }

    /// The imported root keeps its namespace declarations, so the elements carried through
    /// verbatim resolve; the ones this exporter writes are added.
    fn root_tag(&self) -> String {
        const NAME: &str = "office:document-content";
        self.root
            .as_deref()
            .and_then(|r| crate::preserve::merge_root_namespaces(r, NAME, CONTENT_NAMESPACES))
            .unwrap_or_else(|| {
                let mut tag = format!("<{NAME}");
                for (prefix, uri) in CONTENT_NAMESPACES {
                    let _ = write!(tag, r#" xmlns:{prefix}="{uri}""#);
                }
                tag.push('>');
                tag
            })
    }

    /// The carried automatic styles, less any this export generates again (a style that came
    /// from an earlier export), so every name is declared once.
    fn carried_styles(&self, generated: &str) -> String {
        without_styles_named(
            &self.automatic_styles,
            &style_names_in(generated.as_bytes()),
        )
    }
}

/// `styles` (a sequence of style elements) without the top-level ones whose `style:name` is in
/// `names`.
fn without_styles_named(styles: &str, names: &BTreeSet<String>) -> String {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(styles);
    let mut out = String::with_capacity(styles.len());
    let mut depth = 0usize;
    let mut at = 0usize;
    let mut skip_from: Option<usize> = None;
    loop {
        let before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else {
            return styles.to_string();
        };
        let after = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let listed = |e: &quick_xml::events::BytesStart| {
            super::styles::attr_val(e, b"style:name").is_some_and(|n| names.contains(&n))
        };
        match ev {
            Event::Start(e) => {
                if depth == 0 && listed(&e) {
                    skip_from = Some(before);
                }
                depth = depth.saturating_add(1);
            }
            Event::Empty(e) if depth == 0 && listed(&e) => {
                out.push_str(styles.get(at..before).unwrap_or_default());
                at = after;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(start) = skip_from.take()
                {
                    out.push_str(styles.get(at..start).unwrap_or_default());
                    at = after;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out.push_str(styles.get(at..).unwrap_or_default());
    out
}

/// The namespaces the exported `content.xml` declares on its root.
const CONTENT_NAMESPACES: &[(&str, &str)] = &[
    ("office", "urn:oasis:names:tc:opendocument:xmlns:office:1.0"),
    ("text", "urn:oasis:names:tc:opendocument:xmlns:text:1.0"),
    (
        "fo",
        "urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0",
    ),
    ("style", "urn:oasis:names:tc:opendocument:xmlns:style:1.0"),
    ("table", "urn:oasis:names:tc:opendocument:xmlns:table:1.0"),
    ("draw", "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"),
    ("xlink", "http://www.w3.org/1999/xlink"),
    (
        "svg",
        "urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0",
    ),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("meta", "urn:oasis:names:tc:opendocument:xmlns:meta:1.0"),
    (
        "number",
        "urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0",
    ),
];

fn build_content_xml(
    snap: &Resolved<'_>,
    existing_style_names: &BTreeSet<String>,
    head: &ContentHead,
) -> String {
    let mut body = String::new();
    let mut pstyles: Vec<ParaStyleEntry> = Vec::new();
    let mut named_refs: BTreeSet<String> = BTreeSet::new();
    let mut list_styles: Vec<ListStyle> = Vec::new();
    let mut graphic_styles: Vec<(String, String)> = Vec::new();
    let mut inline = InlineCtx {
        used: BTreeMap::new(),
        notes: note_index(snap),
        note_ctr: 0,
        comments: comment_index(snap),
    };
    let (split, tracked_changes) = super::changes::split_tracked_changes(snap);
    let snap = &snap.with_snapshot(split.as_ref());
    let mut style_attr = |b: &BlockView| -> String {
        resolve_para_style(b, head, &snap.styles, &mut pstyles, &mut named_refs)
    };
    let mut i = 0;
    while let Some(b) = snap.blocks.get(i) {
        match b.kind.as_str() {
            "heading" => {
                render_heading(b, &mut body, &mut style_attr, &mut inline);
                i = i.saturating_add(1);
            }
            "list_item" => {
                render_list_run(
                    snap,
                    &mut i,
                    &mut body,
                    head,
                    &mut list_styles,
                    &mut style_attr,
                    &mut inline,
                );
            }
            "image" => {
                // Emit a top-level `draw:frame` referencing the blob written as a `Pictures/<hash>.<ext>`
                // part; size (svg:width/height) and alt (svg:title) round-trip when present.
                if let Some(frame) = image_frame_xml(b, snap, &mut graphic_styles) {
                    body.push_str(&frame);
                }
                i = i.saturating_add(1);
            }
            "fidelity" => {
                // Re-emit the preserved unmodeled element verbatim, in place.
                if let Some(xml) = crate::preserve::fidelity_xml(b, "odt") {
                    body.push_str(xml);
                }
                i = i.saturating_add(1);
            }
            "cell" => {
                render_table_run(snap, &mut i, &mut body, &mut style_attr, &mut inline);
            }
            _ => {
                render_paragraph(b, &mut body, &mut style_attr, &mut inline);
                i = i.saturating_add(1);
            }
        }
    }

    let styles = render_automatic_styles(
        snap,
        existing_style_names,
        &inline.used,
        &named_refs,
        &pstyles,
        &list_styles,
        &graphic_styles,
    );

    let root = head.root_tag();
    let prelude = &head.prelude;
    let carried_styles = head.carried_styles(&styles);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
{root}{prelude}
<office:automatic-styles>{carried_styles}{styles}</office:automatic-styles>
<office:body><office:text>{tracked_changes}{body}</office:text></office:body>
</office:document-content>"#
    )
}

/// Renders a `heading` block as `text:h`, with its outline level clamped to `1..=6`.
fn render_heading(
    b: &BlockView,
    body: &mut String,
    style_attr: &mut impl FnMut(&BlockView) -> String,
    inline: &mut InlineCtx<'_>,
) {
    let level = match b.attrs.get("level") {
        Some(AttrVal::Int(n)) => (*n).clamp(1, 6),
        _ => 1,
    };
    let sa = style_attr(b);
    let _ = write!(
        body,
        r#"<text:h text:outline-level="{level}"{sa}>{}</text:h>"#,
        render_inline(&b.text, &b.marks, inline)
    );
}

/// Renders any block that isn't one of the other recognised kinds as a plain `text:p`.
fn render_paragraph(
    b: &BlockView,
    body: &mut String,
    style_attr: &mut impl FnMut(&BlockView) -> String,
    inline: &mut InlineCtx<'_>,
) {
    let sa = style_attr(b);
    let _ = write!(
        body,
        r"<text:p{sa}>{}</text:p>",
        render_inline(&b.text, &b.marks, inline)
    );
}

/// Renders a run of consecutive `list_item` blocks starting at `*i` as (possibly nested) ODF
/// lists, reconstructing the `text:list`/`text:list-item` nesting from each item's 0-based
/// `level`. A nested list lives inside its parent item, so items stay open until we know
/// whether the next item is deeper; advances `*i` past the run.
fn render_list_run(
    snap: &Resolved<'_>,
    i: &mut usize,
    body: &mut String,
    head: &ContentHead,
    list_styles: &mut Vec<ListStyle>,
    style_attr: &mut impl FnMut(&BlockView) -> String,
    inline: &mut InlineCtx<'_>,
) {
    let mut depth: i64 = 0; // number of <text:list> currently open
    let mut item_open = false; // a <text:list-item> awaiting its close
    let mut top: Option<ListKey> = None; // what the outermost open list encodes
    while let Some(li) = snap.blocks.get(*i).filter(|b| b.kind == "list_item") {
        let target = snap_list_level(li).saturating_add(1); // lists that should be open
        let key = ListKey::of(li);
        // A top-level item of another kind (numbered after bulleted, another format) starts a new list.
        if target == 1 && depth > 0 && top.as_ref() != Some(&key) {
            close_lists(body, &mut depth, &mut item_open);
        }
        if depth == 0 {
            top = Some(key.clone());
        }
        if target > depth {
            // Deeper: open new lists nested inside the current open item. The outermost list,
            // and the innermost one when it differs from it, reference `li`'s list style; the
            // others continue the outer list's style at their level.
            while depth < target {
                depth = depth.saturating_add(1);
                let open = if depth == 1 || (depth == target && top.as_ref() != Some(&key)) {
                    open_list_tag(li, head, list_styles)
                } else {
                    "<text:list>".to_string()
                };
                body.push_str(&open);
            }
        } else {
            // Sibling or shallower: close the current item, then unwind any deeper lists (each wrapped in its parent item).
            if item_open {
                body.push_str("</text:list-item>");
            }
            while depth > target {
                body.push_str("</text:list>");
                depth = depth.saturating_sub(1);
                body.push_str("</text:list-item>");
            }
        }
        let sa = style_attr(li);
        let _ = write!(
            body,
            r"<text:list-item><text:p{sa}>{}</text:p>",
            render_inline(&li.text, &li.marks, inline)
        );
        item_open = true;
        *i = i.saturating_add(1);
    }
    close_lists(body, &mut depth, &mut item_open);
}

/// Close the open list item and every open list.
fn close_lists(body: &mut String, depth: &mut i64, item_open: &mut bool) {
    if std::mem::take(item_open) {
        body.push_str("</text:list-item>");
    }
    while *depth > 0 {
        body.push_str("</text:list>");
        *depth = depth.saturating_sub(1);
        if *depth > 0 {
            body.push_str("</text:list-item>");
        }
    }
}

/// Renders a run of consecutive `cell` blocks belonging to the same table (starting at `*i`)
/// as an ODF `table:table`; advances `*i` past the run.
fn render_table_run(
    snap: &Resolved<'_>,
    i: &mut usize,
    body: &mut String,
    style_attr: &mut impl FnMut(&BlockView) -> String,
    inline: &mut InlineCtx<'_>,
) {
    let Some(first) = snap.blocks.get(*i) else {
        return;
    };
    let tid = snap_cell_table(first).unwrap_or_default();
    let start = *i;
    while snap
        .blocks
        .get(*i)
        .is_some_and(|b| snap_cell_table(b).as_deref() == Some(tid.as_str()))
    {
        *i = i.saturating_add(1);
    }
    let Some(cells) = snap.blocks.get(start..*i) else {
        return;
    };
    let nrows = cells
        .iter()
        .map(|c| snap_cell_rc(c).0)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let ncols = cells
        .iter()
        .map(|c| snap_cell_rc(c).1)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let grid = CellGrid::new(cells);
    let ncols = grid.cols.max(ncols);
    push_table_open(body, &tid, cells.first(), ncols);
    // A row's style rides on its first cell.
    let mut row_styles: BTreeMap<usize, &str> = BTreeMap::new();
    for cell in cells {
        if let Some(AttrVal::Str(s)) = cell.attrs.get(super::content::ODT_ROW_STYLE) {
            row_styles.entry(snap_cell_rc(cell).0).or_insert(s.as_str());
        }
    }
    for r in 0..grid.rows.max(nrows) {
        match row_styles.get(&r) {
            Some(s) => {
                let _ = write!(
                    body,
                    r#"<table:table-row table:style-name="{}">"#,
                    escape_attr(s)
                );
            }
            None => body.push_str("<table:table-row>"),
        }
        for c in 0..ncols {
            if grid.covered.contains(&(r, c)) {
                body.push_str("<table:covered-table-cell/>");
                continue;
            }
            let cell = grid.at.get(&(r, c)).copied();
            let (sa, inner, mut spans) = match cell {
                Some(cl) => (
                    style_attr(cl),
                    render_inline(&cl.text, &cl.marks, inline),
                    cell_spans_xml(cl),
                ),
                None => (String::new(), String::new(), String::new()),
            };
            if let Some(AttrVal::Str(s)) =
                cell.and_then(|cl| cl.attrs.get(super::content::ODT_CELL_STYLE))
            {
                spans.insert_str(0, &format!(r#" table:style-name="{}""#, escape_attr(s)));
            }
            let _ = write!(
                body,
                r"<table:table-cell{spans}><text:p{sa}>{inner}</text:p></table:table-cell>"
            );
        }
        body.push_str("</table:table-row>");
    }
    body.push_str("</table:table>");
}

/// A table's start tag and columns: the style and `table:table-column` elements kept from
/// import on its `first` cell (the columns only while they still number `ncols`), else one
/// run of plain columns.
fn push_table_open(body: &mut String, tid: &str, first: Option<&BlockView>, ncols: usize) {
    let kept = |key: &str| match first.and_then(|c| c.attrs.get(key)) {
        Some(AttrVal::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    let _ = write!(body, r#"<table:table table:name="{}""#, escape_attr(tid));
    if let Some(style) = kept(super::content::ODT_TABLE_STYLE) {
        let _ = write!(body, r#" table:style-name="{}""#, escape_attr(style));
    }
    body.push('>');
    match kept(super::content::ODT_COLUMNS).filter(|cols| column_count(cols) == ncols) {
        Some(cols) => body.push_str(cols),
        None => {
            let _ = write!(
                body,
                r#"<table:table-column table:number-columns-repeated="{ncols}"/>"#
            );
        }
    }
}

/// How many grid columns a run of `table:table-column` elements declares.
fn column_count(columns: &str) -> usize {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(columns);
    let mut n = 0usize;
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"table:table-column" => {
                let repeated = super::styles::attr_val(&e, b"table:number-columns-repeated")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1);
                n = n.saturating_add(repeated);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    n
}

/// A cell's `colspan`/`rowspan` (1 when absent).
fn snap_cell_span(b: &BlockView, key: &str) -> usize {
    match b.attrs.get(key) {
        Some(AttrVal::Int(n)) if *n > 1 => usize::try_from(*n).unwrap_or(1).min(1024),
        _ => 1,
    }
}

/// The spanning attributes of a merged cell.
fn cell_spans_xml(b: &BlockView) -> String {
    let mut out = String::new();
    let (cols, rows) = (snap_cell_span(b, "colspan"), snap_cell_span(b, "rowspan"));
    if cols > 1 {
        let _ = write!(out, r#" table:number-columns-spanned="{cols}""#);
    }
    if rows > 1 {
        let _ = write!(out, r#" table:number-rows-spanned="{rows}""#);
    }
    out
}

/// The cells of one table by grid position, the positions merged cells cover, and the grid
/// size including every span.
struct CellGrid<'a> {
    at: BTreeMap<(usize, usize), &'a BlockView>,
    covered: BTreeSet<(usize, usize)>,
    rows: usize,
    cols: usize,
}

impl<'a> CellGrid<'a> {
    fn new(cells: &'a [BlockView]) -> Self {
        let mut grid = Self {
            at: BTreeMap::new(),
            covered: BTreeSet::new(),
            rows: 0,
            cols: 0,
        };
        for cell in cells {
            let (r, c) = snap_cell_rc(cell);
            let (cols, rows) = (
                snap_cell_span(cell, "colspan"),
                snap_cell_span(cell, "rowspan"),
            );
            grid.at.entry((r, c)).or_insert(cell);
            for dr in 0..rows {
                for dc in 0..cols {
                    if dr > 0 || dc > 0 {
                        grid.covered
                            .insert((r.saturating_add(dr), c.saturating_add(dc)));
                    }
                }
            }
            grid.rows = grid.rows.max(r.saturating_add(rows));
            grid.cols = grid.cols.max(c.saturating_add(cols));
        }
        grid
    }
}

/// Builds the `office:automatic-styles` body: run marks used inline, common named styles
/// referenced by a block's `style` attr (skipped if already declared in a preserved
/// `styles.xml`), per-paragraph resolved styles, list styles, and image-wrap graphic styles.
fn render_automatic_styles(
    snap: &Resolved<'_>,
    existing_style_names: &BTreeSet<String>,
    used: &BTreeMap<String, String>,
    named_refs: &BTreeSet<String>,
    pstyles: &[ParaStyleEntry],
    list_styles: &[ListStyle],
    graphic_styles: &[(String, String)],
) -> String {
    let mut styles = String::new();
    for (name, props) in used {
        let _ = write!(
            styles,
            r#"<style:style style:name="{name}" style:family="text"><style:text-properties {props}/></style:style>"#
        );
    }
    // Named (common) styles referenced by a block's `style` attr, emitted unless already
    // declared in a preserved styles.xml, so the document's CRDT styles render in LibreOffice.
    for id in named_refs {
        if existing_style_names.contains(&super::styles::encode_style_name(id)) {
            continue;
        }
        if let Some(props) = snap.styles.get(id) {
            styles.push_str(&named_style_def_xml(id, props));
        }
    }
    for (idx, (parent, pp, tp)) in pstyles.iter().enumerate() {
        let parent_attr = parent
            .as_ref()
            .map(|p| format!(r#" style:parent-style-name="{p}""#))
            .unwrap_or_default();
        let _ = write!(
            styles,
            r#"<style:style style:name="Pqdoc{}" style:family="paragraph"{parent_attr}><style:paragraph-properties{pp}/><style:text-properties{tp}/></style:style>"#,
            idx.saturating_add(1)
        );
    }
    for ls in list_styles {
        styles.push_str(&ls.xml);
    }
    for (name, wrap) in graphic_styles {
        let _ = write!(
            styles,
            r#"<style:style style:name="{name}" style:family="graphic"><style:graphic-properties style:wrap="{wrap}"/></style:style>"#
        );
    }
    styles
}

#[cfg(test)]
mod tests {
    use super::super::OdtCodec;
    use super::super::container::{OdtContainer, minimal_odt, minimal_odt_styled};
    use crate::FormatProvider;
    use std::collections::BTreeMap;
    use tilo_doc::{AttrVal, DocSnapshot, Document};

    /// Import `bytes` into `doc`, dropping any binary inputs (these tests reference none).
    fn import_odt(bytes: &[u8], doc: &mut Document) {
        OdtCodec
            .import(
                bytes,
                &mut doc.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
    }

    /// Export `doc` with no binary content available.
    fn export_odt(doc: &Document) -> Vec<u8> {
        OdtCodec
            .export(doc.as_ref(), &tilo_content::NoContent)
            .unwrap()
    }

    type Norm = Vec<(
        String,
        String,
        Vec<(usize, usize, String)>,
        BTreeMap<String, AttrVal>,
    )>;

    fn norm(snap: &DocSnapshot) -> Norm {
        snap.blocks
            .iter()
            .map(|b| {
                let mut m: Vec<_> = b
                    .marks
                    .iter()
                    .map(|x| (x.start, x.end, x.name.clone()))
                    .collect();
                m.sort();
                (b.kind.clone(), b.text.clone(), m, b.attrs.clone())
            })
            .collect()
    }

    /// One block's `(text, marks-with-values)`, marks as `(start, end, name, value)`.
    type NormalizedRegionBlock = (
        String,
        Vec<(usize, usize, String, tilo_doc::document::MarkValue)>,
    );

    /// Normalize a region's blocks to `(text, marks-with-values)`, dropping the volatile per-import block id so two independent imports compare equal.
    fn norm_region(blocks: &[tilo_doc::snapshot::BlockView]) -> Vec<NormalizedRegionBlock> {
        blocks
            .iter()
            .map(|b| {
                let mut m: Vec<_> = b
                    .marks
                    .iter()
                    .map(|x| (x.start, x.end, x.name.clone(), x.value.clone()))
                    .collect();
                m.sort();
                (b.text.clone(), m)
            })
            .collect()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn text_and_paragraph_formatting_roundtrips_through_odt() {
        // Phase `export_import_roundtrips_model`.
        {
            let styles = r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#;
            let body = concat!(
                r#"<text:h text:outline-level="1">Title</text:h>"#,
                r#"<text:p>a <text:span text:style-name="T1">bold</text:span> b</text:p>"#,
                r#"<text:list><text:list-item><text:p>one</text:p></text:list-item>"#,
                r#"<text:list-item><text:p>two</text:p></text:list-item></text:list>"#,
            );
            let odt = minimal_odt_styled(styles, body);

            let mut d = Document::new();
            import_odt(&odt, &mut d);
            let m1 = norm(&d.materialize());

            let exported = export_odt(&d);
            let c = OdtContainer::read(&exported).unwrap();
            assert_eq!(c.parts[0].0, "mimetype", "exported .odt has mimetype first");
            assert!(c.get("content.xml").is_some());

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let m2 = norm(&d2.materialize());
            assert_eq!(m1, m2, "model round-trips through .odt export then import");
        }
        // Phase `paragraph_props_roundtrip`.
        {
            use tilo_doc::document::MarkValue;
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "styled");
            {
                let mut dm = d.as_mut();
                dm.set_attr(&id, "align", "center");
                dm.add_mark(&id, 0, 6, "fontSize", 22_i64);
                dm.add_mark(&id, 0, 6, "fontFamily", "serif");
            }

            let exported = export_odt(&d);
            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            let b = &snap.blocks[0];
            assert_eq!(b.text, "styled");
            assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())));
            assert!(
                b.marks
                    .iter()
                    .any(|m| m.name == "fontSize" && m.value == MarkValue::Int(22)),
                "{:?}",
                b.marks
            );
            assert!(
                b.marks
                    .iter()
                    .any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())),
                "{:?}",
                b.marks
            );
        }
        // Phase `highlight_super_and_line_spacing_roundtrip`.
        {
            use tilo_doc::document::MarkValue;
            // A run with `highlight` + a `super` run + a paragraph-level `line_spacing` attr must all survive the .odt export → import cycle.
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "E=mc2");
            {
                let mut dm = d.as_mut();
                dm.add_mark(&id, 0, 5, "highlight", "#ffff00"); // whole run highlighted
                dm.add_mark(&id, 4, 5, "super", true); // the exponent
                dm.set_attr(&id, "line_spacing", 1.5_f64);
            }

            let exported = export_odt(&d);
            let c = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(c.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains(r##"fo:background-color="#ffff00""##),
                "highlight bg: {content}"
            );
            assert!(
                content.contains(r#"style:text-position="super"#),
                "super position: {content}"
            );
            assert!(
                content.contains(r#"fo:line-height="150%""#),
                "line-height: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            let b = &snap.blocks[0];
            assert_eq!(b.text, "E=mc2");
            assert!(
                b.marks.iter().any(|m| m.name == "highlight"
                    && m.value == MarkValue::Str("#ffff00".into())
                    && m.start == 0
                    && m.end == 5),
                "highlight mark: {:?}",
                b.marks
            );
            assert!(
                b.marks.iter().any(|m| m.name == "super"
                    && m.value == MarkValue::Bool(true)
                    && m.start == 4
                    && m.end == 5),
                "super mark: {:?}",
                b.marks
            );
            assert_eq!(
                b.attrs.get("line_spacing"),
                Some(&AttrVal::Float(1.5)),
                "line_spacing attr: {:?}",
                b.attrs
            );
        }
        // Phase `subscript_roundtrips_through_odt`.
        {
            use tilo_doc::document::MarkValue;
            // A `sub` run exports as `style:text-position="sub …"` and re-imports as `sub`.
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "H2O");
            d.as_mut().add_mark(&id, 1, 2, "sub", true);

            let exported = export_odt(&d);
            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            let b = &snap.blocks[0];
            assert!(
                b.marks.iter().any(|m| m.name == "sub"
                    && m.value == MarkValue::Bool(true)
                    && m.start == 1
                    && m.end == 2),
                "sub mark: {:?}",
                b.marks
            );
        }
        // Phase `text_color_roundtrips`.
        {
            use tilo_doc::document::MarkValue;
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "red text");
            d.as_mut().add_mark(&id, 0, 3, "color", "#ff0000");

            let exported = export_odt(&d);
            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let b = &d2.materialize().blocks[0];
            assert_eq!(b.text, "red text");
            assert!(
                b.marks.iter().any(|m| m.name == "color"
                    && m.start == 0
                    && m.end == 3
                    && m.value == MarkValue::Str("#ff0000".into())),
                "color survives round-trip: {:?}",
                b.marks
            );
        }
        // Phase `colored_serif_20pt_justified_run_roundtrips_through_odt`.
        {
            use tilo_doc::document::MarkValue;
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
            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains(r#"fo:text-align="justify""#),
                "justify → fo:text-align justify: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let b = &d2.materialize().blocks[0];
            assert_eq!(b.text, "styled");
            assert_eq!(
                b.attrs.get("align"),
                Some(&AttrVal::Str("justify".into())),
                "justify survives: {:?}",
                b.attrs
            );
            assert!(
                b.marks
                    .iter()
                    .any(|m| m.name == "color" && m.value == MarkValue::Str("#3366cc".into())),
                "colour: {:?}",
                b.marks
            );
            assert!(
                b.marks
                    .iter()
                    .any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())),
                "serif: {:?}",
                b.marks
            );
            assert!(
                b.marks
                    .iter()
                    .any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)),
                "20pt: {:?}",
                b.marks
            );
        }
        // Phase `named_paragraph_style_roundtrips_through_odt`.
        {
            // A paragraph referencing a custom CRDT-native named style must export as a real ODF
            // `style:style` (family paragraph) + `text:style-name`, and re-import with it still applied.
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

            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains(r#"text:style-name="Fancy""#),
                "paragraph references the named style: {content}"
            );
            assert!(
                content.contains(r#"<style:style style:name="Fancy" style:family="paragraph""#),
                "named style defined as ODF style:style: {content}"
            );
            assert!(
                content.contains(r#"fo:font-weight="bold""#)
                    && content.contains(r#"fo:font-size="20pt""#),
                "props emitted: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            let b = &snap.blocks[0];
            assert_eq!(b.text, "fancy");
            assert_eq!(
                b.attrs.get("style"),
                Some(&AttrVal::Str("Fancy".into())),
                "block keeps its style ref: {:?}",
                b.attrs
            );

            let st = snap
                .styles
                .get("Fancy")
                .expect("style present in container after round-trip");
            assert_eq!(st.get("name"), Some(&AttrVal::Str("Fancy".into())));
            assert_eq!(st.get("font"), Some(&AttrVal::Str("serif".into())));
            assert_eq!(st.get("size"), Some(&AttrVal::Float(20.0)));
            assert_eq!(st.get("bold"), Some(&AttrVal::Bool(true)));
            assert_eq!(st.get("italic"), Some(&AttrVal::Bool(true)));
            assert_eq!(st.get("color"), Some(&AttrVal::Str("#ff0000".into())));
            assert_eq!(st.get("align"), Some(&AttrVal::Str("center".into())));
            assert_eq!(st.get("space_after"), Some(&AttrVal::Float(12.0)));

            let exported2 = export_odt(&d2);
            let mut d3 = Document::new();
            import_odt(&exported2, &mut d3);
            assert_eq!(
                norm(&d2.materialize()),
                norm(&d3.materialize()),
                "named-style model is stable across a second round-trip"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn hyperlinks_and_cross_references_roundtrip_through_odt() {
        // Phase `hyperlink_roundtrips`.
        {
            use tilo_doc::document::MarkValue;
            // A `link` mark exports as `text:a` and re-imports as the same mark+href.
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "go home now");
            d.as_mut().add_mark(&id, 3, 7, "link", "https://tilo.so/");

            let exported = export_odt(&d);
            let c = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(c.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains(r#"<text:a xlink:href="https://tilo.so/">home</text:a>"#),
                "exported a text:a element: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let b = &d2.materialize().blocks[0];
            assert_eq!(b.text, "go home now");
            let link = b
                .marks
                .iter()
                .find(|m| m.name == "link")
                .expect("link survives round-trip");
            assert_eq!((link.start, link.end), (3, 7), "{:?}", b.marks);
            assert_eq!(link.value, MarkValue::Str("https://tilo.so/".into()));
        }
        // Phase `bookmark_and_cross_reference_roundtrip`.
        {
            use tilo_doc::document::MarkValue;
            // A bookmark "intro" over a word + a cross-reference pointing at it must survive an `.odt` export+import with both names and ranges intact.
            let mut d = Document::new();
            let target = d.insert_block(None, "paragraph");
            d.splice_text(&target, 0, 0, "the intro section");
            d.add_bookmark(&target, 4, 9, "intro"); // over "intro"
            let src = d.insert_block(Some(&target), "paragraph");
            d.splice_text(&src, 0, 0, "see above");
            d.add_cross_ref(&src, 4, 9, "intro"); // "above" → #intro

            let exported = export_odt(&d);
            let c = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(c.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains(r#"<text:bookmark-start text:name="intro"/>"#),
                "bookmark-start emitted: {content}"
            );
            assert!(
                content.contains(r#"<text:bookmark-end text:name="intro"/>"#),
                "bookmark-end emitted: {content}"
            );
            assert!(
                content.contains(r##"<text:a xlink:href="#intro">above</text:a>"##),
                "cross-ref text:a emitted: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            assert_eq!(
                snap.bookmarks.len(),
                1,
                "one bookmark survives: {:?}",
                snap.bookmarks
            );
            let bm = &snap.bookmarks[0];
            assert_eq!(bm.name, "intro");
            assert_eq!(
                (bm.range.1, bm.range.2),
                (4, 9),
                "bookmark range over 'intro': {bm:?}"
            );
            let tb = snap
                .blocks
                .iter()
                .find(|b| b.text == "the intro section")
                .unwrap();
            let mark = tb
                .marks
                .iter()
                .find(|m| m.name == tilo_doc::schema::MARK_BOOKMARK)
                .expect("bookmark mark present");
            assert_eq!((mark.start, mark.end), (4, 9));
            assert_eq!(mark.value, MarkValue::Str("intro".into()));
            let sb = snap.blocks.iter().find(|b| b.text == "see above").unwrap();
            let link = sb
                .marks
                .iter()
                .find(|m| m.name == "link")
                .expect("cross-ref link mark present");
            assert_eq!(
                (link.start, link.end),
                (4, 9),
                "cross-ref over 'above': {:?}",
                sb.marks
            );
            assert_eq!(link.value, MarkValue::Str("#intro".into()));
        }
        // Phase `hyperlink_escapes_href`.
        {
            // An href with `&` must be attribute-escaped on export and unescaped on import.
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "link");
            d.as_mut().add_mark(&id, 0, 4, "link", "https://x/?a=1&b=2");

            let exported = export_odt(&d);
            let c = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(c.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains("a=1&amp;b=2"),
                "ampersand escaped: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let link = d2.materialize().blocks[0]
                .marks
                .iter()
                .find(|m| m.name == "link")
                .unwrap()
                .value
                .clone();
            assert_eq!(
                link,
                tilo_doc::document::MarkValue::Str("https://x/?a=1&b=2".into())
            );
        }
        // Phase `link_and_style_marks_nest_well`.
        {
            use tilo_doc::document::MarkValue;
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
            let exported = export_odt(&d);
            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let b = &d2.materialize().blocks[0];
            assert_eq!(b.text, "click");
            for name in ["link", "strong", "color"] {
                assert!(
                    b.marks
                        .iter()
                        .any(|m| m.name == name && m.start == 0 && m.end == 5),
                    "{name} survives: {:?}",
                    b.marks
                );
            }
            assert_eq!(
                b.marks.iter().find(|m| m.name == "link").unwrap().value,
                MarkValue::Str("http://q/".into())
            );
        }
    }

    /// A genuinely valid 1x1 truecolor-RGB PNG (signature + IHDR + IDAT + IEND),
    /// embedded so the image round-trip is self-contained.
    const TINY_PNG: [u8; 69] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xf7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn images_roundtrip_through_odt() {
        // Phase `image_block_roundtrips_through_odt`.
        {
            let mut d = Document::new();
            let p1 = d.insert_block(None, "paragraph");
            d.splice_text(&p1, 0, 0, "before");
            let img = d.insert_block(Some(&p1), "image");
            let p3 = d.insert_block(Some(&img), "paragraph");
            d.splice_text(&p3, 0, 0, "after");
            let mut inputs = tilo_content::ContentInputs::new();
            let hash = d.attach(&TINY_PNG, &mut inputs);
            {
                let mut dm = d.as_mut();
                dm.set_attr(&img, "blob", hash.as_str());
                dm.set_attr(&img, "alt", "a tiny red dot");
                dm.set_attr(&img, "width", 64_i64);
                dm.set_attr(&img, "height", 48_i64);
            }

            let exported = OdtCodec.export(d.as_ref(), &inputs).unwrap();

            let cont = OdtContainer::read(&exported).unwrap();
            let pic = cont
                .parts
                .iter()
                .find(|(n, _)| {
                    n.starts_with("Pictures/") && n.to_ascii_lowercase().ends_with(".png")
                })
                .expect("Pictures/<hash>.png written");
            assert_eq!(pic.1, &TINY_PNG[..], "picture bytes written verbatim");
            let manifest = std::str::from_utf8(cont.get("META-INF/manifest.xml").unwrap()).unwrap();
            assert!(
                manifest.contains(&pic.0) && manifest.contains("image/png"),
                "picture registered in manifest: {manifest}"
            );
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains("<draw:frame"),
                "draw:frame emitted: {content}"
            );
            assert!(
                content.contains(&format!(r#"xlink:href="{}""#, pic.0)),
                "image href matches part: {content}"
            );

            let mut d2 = Document::new();
            let mut reimported = tilo_content::ContentInputs::new();
            OdtCodec
                .import(&exported, &mut d2.as_mut(), &mut reimported)
                .unwrap();
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
            assert_eq!(
                im.attrs.get("alt"),
                Some(&AttrVal::Str("a tiny red dot".into()))
            );
            assert_eq!(
                im.attrs.get("width"),
                Some(&AttrVal::Int(64)),
                "{:?}",
                im.attrs
            );
            assert_eq!(
                im.attrs.get("height"),
                Some(&AttrVal::Int(48)),
                "{:?}",
                im.attrs
            );

            assert_eq!(
                reimported.get(&blob_hash),
                Some(&TINY_PNG[..]),
                "image bytes survive the round-trip"
            );
        }
        // Phase `image_positioning_roundtrips_through_odt`.
        {
            let mut d = Document::new();
            let p1 = d.insert_block(None, "paragraph");
            d.splice_text(&p1, 0, 0, "text");
            let img = d.insert_block(Some(&p1), "image");
            let mut inputs = tilo_content::ContentInputs::new();
            let hash = d.attach(&TINY_PNG, &mut inputs);
            {
                let mut dm = d.as_mut();
                dm.set_attr(&img, "blob", hash.as_str());
                dm.set_attr(&img, "anchor", "paragraph");
                dm.set_attr(&img, "wrap", "right");
                dm.set_attr(&img, "width", 120_i64);
                dm.set_attr(&img, "height", 80_i64);
            }

            let exported = OdtCodec.export(d.as_ref(), &inputs).unwrap();
            let cont = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains(r#"text:anchor-type="paragraph""#),
                "anchor-type emitted: {content}"
            );
            assert!(
                content.contains(r#"style:family="graphic""#)
                    && content.contains(r#"style:wrap="right""#),
                "graphic style with wrap emitted: {content}"
            );

            let mut d2 = Document::new();
            let mut reimported = tilo_content::ContentInputs::new();
            OdtCodec
                .import(&exported, &mut d2.as_mut(), &mut reimported)
                .unwrap();
            let snap = d2.materialize();
            let im = snap
                .blocks
                .iter()
                .find(|b| b.kind == "image")
                .expect("image block present");
            assert_eq!(
                im.attrs.get("anchor"),
                Some(&AttrVal::Str("paragraph".into())),
                "{:?}",
                im.attrs
            );
            assert_eq!(
                im.attrs.get("wrap"),
                Some(&AttrVal::Str("right".into())),
                "{:?}",
                im.attrs
            );
            assert_eq!(
                im.attrs.get("width"),
                Some(&AttrVal::Int(120)),
                "{:?}",
                im.attrs
            );
            assert_eq!(
                im.attrs.get("height"),
                Some(&AttrVal::Int(80)),
                "{:?}",
                im.attrs
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn lists_roundtrip_through_odt() {
        // Phase `nested_list_levels_roundtrip_through_odt`.
        {
            let mut d = Document::new();
            let item_a = d.insert_block(None, "list_item");
            d.splice_text(&item_a, 0, 0, "a");
            let item_b = d.insert_block(Some(&item_a), "list_item");
            d.splice_text(&item_b, 0, 0, "b");
            let nested = d.insert_block(Some(&item_b), "list_item");
            d.splice_text(&nested, 0, 0, "nested");
            d.as_mut().set_attr(&nested, "level", 1_i64);
            let item_c = d.insert_block(Some(&nested), "list_item");
            d.splice_text(&item_c, 0, 0, "c");

            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert!(
            content.contains(r"<text:list-item><text:p>b</text:p><text:list><text:list-item><text:p>nested</text:p></text:list-item></text:list></text:list-item>"),
            "nested list emitted inside its parent item: {content}"
        );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            let level = |b: &tilo_doc::snapshot::BlockView| match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => *n,
                _ => 0i64,
            };
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .map(|b| (b.text.clone(), level(b)))
                .collect();
            assert_eq!(
                items,
                vec![
                    ("a".into(), 0i64),
                    ("b".into(), 0),
                    ("nested".into(), 1),
                    ("c".into(), 0)
                ],
                "levels round-trip through nested ODF lists"
            );
        }
        // Phase `list_numbering_format_and_start_roundtrip_through_odt`.
        {
            // An upper-roman ordered list starting at 3 must survive export then re-import — fidelity
            // preservation: the editor renders decimal, but the numbering metadata must not be lost.
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

            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains(r#"style:num-format="I""#),
                "upper-roman list-style emitted: {content}"
            );
            assert!(
                content.contains(r#"text:start-value="3""#),
                "start value emitted: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|b| b.kind == "list_item")
                .collect();
            assert_eq!(items.len(), 2, "{snap:?}");
            for it in &items {
                assert_eq!(
                    it.attrs.get("ordered"),
                    Some(&AttrVal::Bool(true)),
                    "still ordered: {:?}",
                    it.attrs
                );
                assert_eq!(
                    it.attrs.get("num_format"),
                    Some(&AttrVal::Str("upper-roman".into())),
                    "format survives: {:?}",
                    it.attrs
                );
                assert_eq!(
                    it.attrs.get("num_start"),
                    Some(&AttrVal::Int(3)),
                    "start survives: {:?}",
                    it.attrs
                );
            }

            let exported2 = export_odt(&d2);
            let mut d3 = Document::new();
            import_odt(&exported2, &mut d3);
            assert_eq!(
                norm(&d2.materialize()),
                norm(&d3.materialize()),
                "list-numbering model stable across a second round-trip"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn document_properties_and_preserved_parts_roundtrip_through_odt() {
        // Phase `document_properties_roundtrip_through_odt`.
        {
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

            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let meta =
                std::str::from_utf8(cont.get("meta.xml").expect("meta.xml written")).unwrap();
            assert!(
                meta.contains("<dc:title>My Title</dc:title>"),
                "title in meta.xml: {meta}"
            );
            assert!(
                meta.contains("<dc:creator>Ada Lovelace</dc:creator>"),
                "creator: {meta}"
            );
            assert!(
                meta.contains("<dc:subject>Analytical Engine</dc:subject>"),
                "subject: {meta}"
            );
            assert!(
                meta.contains("<meta:keyword>computing, history</meta:keyword>"),
                "keyword: {meta}"
            );
            assert!(
                meta.contains("<meta:creation-date>2020-01-02T03:04:05</meta:creation-date>"),
                "created: {meta}"
            );
            assert!(
                meta.contains("<dc:date>2021-06-07T08:09:10</dc:date>"),
                "modified: {meta}"
            );
            assert!(
                meta.contains("<meta:generator>qdoc/1.0</meta:generator>"),
                "generator: {meta}"
            );
            let manifest = std::str::from_utf8(cont.get("META-INF/manifest.xml").unwrap()).unwrap();
            assert!(
                manifest.contains("meta.xml"),
                "meta.xml registered in manifest: {manifest}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let props = d2.doc_props();
            assert_eq!(props.get("title").map(String::as_str), Some("My Title"));
            assert_eq!(
                props.get("author").map(String::as_str),
                Some("Ada Lovelace")
            );
            assert_eq!(
                props.get("subject").map(String::as_str),
                Some("Analytical Engine")
            );
            assert_eq!(
                props.get("keywords").map(String::as_str),
                Some("computing, history")
            );
            assert_eq!(
                props.get("created").map(String::as_str),
                Some("2020-01-02T03:04:05")
            );
            assert_eq!(
                props.get("modified").map(String::as_str),
                Some("2021-06-07T08:09:10")
            );
            assert_eq!(props.get("generator").map(String::as_str), Some("qdoc/1.0"));
            assert_eq!(d2.materialize().blocks[0].text, "body");

            let exported2 = export_odt(&d2);
            let mut d3 = Document::new();
            import_odt(&exported2, &mut d3);
            assert_eq!(
                d2.doc_props(),
                d3.doc_props(),
                "properties stable across a second round-trip"
            );
        }
        // Phase `meta_xml_preserves_unmodeled_fields_and_updates_modeled`.
        {
            // An imported meta.xml with an unmodeled field (editing-cycles) plus an existing title: the unmodeled field survives and the title updates.
            let meta = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:meta><dc:title>Old</dc:title><meta:editing-cycles>7</meta:editing-cycles></office:meta></office:document-meta>"#;
            let base = minimal_odt_styled("", "<text:p>hi</text:p>");
            let mut c = OdtContainer::read(&base).unwrap();
            c.set("meta.xml", meta.as_bytes().to_vec());
            let odt = c.write().unwrap();

            let mut d = Document::new();
            import_odt(&odt, &mut d);
            assert_eq!(
                d.doc_prop("title").as_deref(),
                Some("Old"),
                "title imported"
            );
            d.set_doc_prop("title", "New");
            d.set_doc_prop("keywords", "k");

            let exported = export_odt(&d);
            let out = OdtContainer::read(&exported).unwrap();
            let meta_out = std::str::from_utf8(out.get("meta.xml").unwrap()).unwrap();
            assert!(
                meta_out.contains("<dc:title>New</dc:title>"),
                "title updated: {meta_out}"
            );
            assert!(
                !meta_out.contains("<dc:title>Old</dc:title>"),
                "old title gone: {meta_out}"
            );
            assert!(
                meta_out.contains("editing-cycles>7</"),
                "unmodeled field preserved: {meta_out}"
            );
            assert!(
                meta_out.contains("<meta:keyword>k</meta:keyword>"),
                "new keyword emitted: {meta_out}"
            );
        }
        // Phase `no_meta_part_when_no_properties_and_none_imported`.
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "plain");
            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            assert!(
                cont.get("meta.xml").is_none(),
                "no metadata part synthesised for a doc with no properties"
            );
        }
        // Phase `preserves_unmodeled_parts`.
        {
            let base = minimal_odt_styled("", "<text:p>hi</text:p>");
            let mut c = OdtContainer::read(&base).unwrap();
            c.set(
                "settings.xml",
                b"<office:settings>custom</office:settings>".to_vec(),
            );
            let odt = c.write().unwrap();

            let mut d = Document::new();
            import_odt(&odt, &mut d);
            let exported = export_odt(&d);

            let out = OdtContainer::read(&exported).unwrap();
            assert_eq!(
                out.get("settings.xml"),
                Some(&b"<office:settings>custom</office:settings>"[..]),
                "unmodeled part must round-trip byte-identically"
            );
            assert_eq!(out.parts[0].0, "mimetype", "mimetype still first");
            assert!(out.get("content.xml").is_some(), "content regenerated");
        }
        // Phase `preserved_multi_atom_part_round_trips_verbatim`.
        // Round-trip fidelity: a preserved multi-atom `styles.xml` re-emits verbatim through the full
        // import→export codec path even though it is now stored as per-atom segments, not one blob.
        {
            let odt = odt_with_two_style_atoms();
            let original_styles = OdtContainer::read(&odt)
                .unwrap()
                .get("styles.xml")
                .unwrap()
                .to_vec();

            let mut d = Document::new();
            import_odt(&odt, &mut d);
            let exported = export_odt(&d);
            let out = OdtContainer::read(&exported).unwrap();
            assert_eq!(
                out.get("styles.xml").unwrap(),
                original_styles.as_slice(),
                "segmented styles.xml re-emits byte-for-byte"
            );
            // The part is stored as two addressable style atoms.
            let r = d.as_ref();
            assert!(
                tilo_doc::model::preserved_atoms(r.doc, r.c, "odt/part/styles.xml").len() >= 2,
                "styles.xml stored as multiple per-atom segments"
            );
        }
        // Phase `different_preserved_atom_edits_converge_through_codec`.
        // Two replicas import the same document, each rewrites a different
        // preserved style atom, and both edits survive in the exported
        // `styles.xml`. A whole-part `Bytes` scalar would overwrite the other edit.
        {
            let odt = odt_with_two_style_atoms();

            // Replica A imports; replica B forks A (so they share atom ids).
            let mut a = Document::new();
            import_odt(&odt, &mut a);
            let mut b = a.fork();

            // The two style atoms have the SAME stable ids on both replicas.
            let (id_quote, quote_bytes) = atom_containing(&a, "QuoteFont");
            let (id_caption, caption_bytes) = atom_containing(&a, "CaptionFont");
            assert_eq!(
                id_quote,
                atom_containing(&b, "QuoteFont").0,
                "fork shares atom ids"
            );
            assert_eq!(
                id_caption,
                atom_containing(&b, "CaptionFont").0,
                "fork shares atom ids"
            );
            assert_ne!(id_quote, id_caption, "distinct atoms have distinct ids");

            // A rewrites ONLY the Quote atom; B rewrites ONLY the Caption atom — each a read-modify-write
            // that keeps the atom's framing (prologue/root-open tags on the first atom, close tags on the last).
            let edited_quote = String::from_utf8(quote_bytes)
                .unwrap()
                .replace("QuoteFont", "QuoteFont-EDITED");
            let edited_caption = String::from_utf8(caption_bytes)
                .unwrap()
                .replace("CaptionFont", "CaptionFont-EDITED");
            a.as_mut().set_preserved_atom_bytes(
                "odt/part/styles.xml",
                &id_quote,
                edited_quote.as_bytes(),
            );
            b.as_mut().set_preserved_atom_bytes(
                "odt/part/styles.xml",
                &id_caption,
                edited_caption.as_bytes(),
            );

            a.merge(&mut b);
            b.merge(&mut a);

            // Both replicas export an identical styles.xml that carries BOTH edits.
            for (d, who) in [(&a, "A"), (&b, "B")] {
                let exported = export_odt(d);
                let out = OdtContainer::read(&exported).unwrap();
                let styles = String::from_utf8(out.get("styles.xml").unwrap().to_vec()).unwrap();
                assert!(
                    styles.contains("QuoteFont-EDITED"),
                    "replica {who} kept A's Quote edit: {styles}"
                );
                assert!(
                    styles.contains("CaptionFont-EDITED"),
                    "replica {who} kept B's Caption edit: {styles}"
                );
                // Order + structure intact (Quote before Caption, root tags present).
                assert!(
                    styles.find("QuoteFont-EDITED").unwrap()
                        < styles.find("CaptionFont-EDITED").unwrap(),
                    "replica {who} kept atom order: {styles}"
                );
                assert!(
                    styles.contains("<office:styles>")
                        && styles.contains("</office:document-styles>")
                );
            }
            // Convergence: identical exported styles.xml on both replicas.
            let sa = OdtContainer::read(&export_odt(&a))
                .unwrap()
                .get("styles.xml")
                .unwrap()
                .to_vec();
            let sb = OdtContainer::read(&export_odt(&b))
                .unwrap()
                .get("styles.xml")
                .unwrap()
                .to_vec();
            assert_eq!(sa, sb, "replicas converge on identical styles.xml");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn headers_and_footers_roundtrip_through_odt() {
        // Phase `header_footer_with_page_number_roundtrip_through_odt`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_FIELD};
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "body");
            let h = d.insert_header_block(None, "paragraph");
            d.splice_header_text(&h, 0, 0, "Title");
            let f = d.insert_footer_block(None, "paragraph");
            d.splice_footer_text(&f, 0, 0, "Page ");
            d.insert_footer_page_number(&f, 5);

            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let styles =
                std::str::from_utf8(cont.get("styles.xml").expect("styles.xml written")).unwrap();
            assert!(
                styles.contains("<style:header>") && styles.contains("Title"),
                "header in master-page: {styles}"
            );
            assert!(
                styles.contains("<style:footer>"),
                "footer in master-page: {styles}"
            );
            assert!(
                styles.contains("<text:page-number/>"),
                "page-number field emitted: {styles}"
            );
            let manifest = std::str::from_utf8(cont.get("META-INF/manifest.xml").unwrap()).unwrap();
            assert!(
                manifest.contains("styles.xml"),
                "styles.xml registered in manifest: {manifest}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            assert_eq!(d2.materialize().blocks[0].text, "body");
            let header = d2.header();
            assert_eq!(header.len(), 1, "header block present: {header:?}");
            assert_eq!(header[0].text, "Title");
            let footer = d2.footer();
            assert_eq!(footer.len(), 1, "footer block present: {footer:?}");
            assert_eq!(
                footer[0].text,
                format!("Page {FIELD_PLACEHOLDER}"),
                "placeholder kept: {:?}",
                footer[0].text
            );
            let field = footer[0]
                .marks
                .iter()
                .find(|m| m.name == MARK_FIELD)
                .expect("page-number field survives");
            assert_eq!(
                (field.start, field.end),
                (5, 6),
                "field spans the placeholder: {:?}",
                footer[0].marks
            );
            assert_eq!(field.value, MarkValue::Str(FIELD_PAGE_NUMBER.into()));

            // Idempotent across a second round-trip: block ids are volatile per import, so compare on text + marks, not the full BlockView.
            let exported2 = export_odt(&d2);
            let mut d3 = Document::new();
            import_odt(&exported2, &mut d3);
            assert_eq!(
                norm_region(&d2.header()),
                norm_region(&d3.header()),
                "header stable across a second round-trip"
            );
            assert_eq!(
                norm_region(&d2.footer()),
                norm_region(&d3.footer()),
                "footer stable across a second round-trip"
            );
        }
        // Phase `header_footer_preserve_existing_master_page_content`.
        {
            // An imported styles.xml whose master-page references a page-layout must keep that
            // reference (+ any other master-page content) when the header/footer are re-emitted.
            let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="pm1"><style:header><text:p>Old Header</text:p></style:header></style:master-page></office:master-styles></office:document-styles>"#;
            let base = minimal_odt_styled("", "<text:p>hi</text:p>");
            let mut c = OdtContainer::read(&base).unwrap();
            c.set("styles.xml", styles_xml.as_bytes().to_vec());
            let odt = c.write().unwrap();

            let mut d = Document::new();
            import_odt(&odt, &mut d);
            assert_eq!(d.header()[0].text, "Old Header", "existing header imported");
            let hid = d.header_blocks_in_order()[0].clone();
            d.splice_header_text(&hid, 0, 9, "New Header"); // replace "Old Header"

            let exported = export_odt(&d);
            let out = OdtContainer::read(&exported).unwrap();
            let styles_out = std::str::from_utf8(out.get("styles.xml").unwrap()).unwrap();
            assert!(
                styles_out.contains(r#"style:page-layout-name="pm1""#),
                "page-layout reference preserved: {styles_out}"
            );
            assert!(
                styles_out.contains("New Header") && !styles_out.contains("Old Header"),
                "header updated: {styles_out}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn notes_comments_and_tracked_changes_roundtrip_through_odt() {
        // Phase `footnote_imports_from_inline_text_note`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::{FIELD_PLACEHOLDER, MARK_NOTE_REF};
            // A paragraph "see ref" with an inline footnote right after "see ": the body gets a `note-ref` marker over a placeholder, and the note's body becomes a note in the `notes` container.
            let body = concat!(
                r#"<text:p>see <text:note text:id="ftn1" text:note-class="footnote">"#,
                r#"<text:note-citation>1</text:note-citation>"#,
                r#"<text:note-body><text:p>the footnote text</text:p></text:note-body>"#,
                r#"</text:note>ref</text:p>"#,
            );
            let odt = minimal_odt(body);
            let mut d = Document::new();
            import_odt(&odt, &mut d);
            let snap = d.materialize();

            let p = &snap.blocks[0];
            assert_eq!(
                p.text,
                format!("see {FIELD_PLACEHOLDER}ref"),
                "placeholder spliced inline: {:?}",
                p.text
            );
            let nref = p
                .marks
                .iter()
                .find(|m| m.name == MARK_NOTE_REF)
                .expect("note-ref mark present");
            assert_eq!(
                (nref.start, nref.end),
                (4, 5),
                "marker covers exactly the placeholder: {:?}",
                p.marks
            );
            let nid = match &nref.value {
                MarkValue::Str(s) => s.clone(),
                other => panic!("note-ref value is the note id, got {other:?}"),
            };

            assert_eq!(snap.notes.len(), 1, "one note: {:?}", snap.notes);
            let note = &snap.notes[0];
            assert_eq!(note.id, nid, "marker references the note by id");
            assert_eq!(note.kind, "footnote");
            assert_eq!(note.blocks.len(), 1);
            assert_eq!(note.blocks[0].text, "the footnote text");
        }
        // Phase `footnote_and_endnote_roundtrip_through_odt`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::MARK_NOTE_REF;
            // Build from scratch: a paragraph "see ref" with a footnote after "see ", and a second
            // paragraph with an endnote — both must export as inline `text:note` and re-import intact.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "see ref");
            let fnote_id = d.insert_footnote(&p, 4); // after "see "
            let fnote_block = d.insert_note_block(&fnote_id, None, "paragraph").unwrap();
            d.splice_note_text(&fnote_id, &fnote_block, 0, 0, "a footnote");

            let p2 = d.insert_block(Some(&p), "paragraph");
            d.splice_text(&p2, 0, 0, "tail");
            let enote_id = d.insert_endnote(&p2, 4); // at the end
            let enote_block = d.insert_note_block(&enote_id, None, "paragraph").unwrap();
            d.splice_note_text(&enote_id, &enote_block, 0, 0, "an endnote");

            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains(r#"text:note-class="footnote""#),
                "footnote emitted inline: {content}"
            );
            assert!(
                content.contains(r#"text:note-class="endnote""#),
                "endnote emitted inline: {content}"
            );
            assert!(
                content.contains("a footnote") && content.contains("an endnote"),
                "note bodies emitted: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            assert_eq!(
                snap.blocks[0]
                    .text
                    .chars()
                    .filter(|c| *c != '\u{FFFC}')
                    .collect::<String>(),
                "see ref"
            );
            let nref = snap.blocks[0]
                .marks
                .iter()
                .find(|m| m.name == MARK_NOTE_REF)
                .expect("footnote marker survives");
            let f_id = match &nref.value {
                MarkValue::Str(s) => s.clone(),
                _ => panic!("note-ref value"),
            };
            assert_eq!(snap.notes.len(), 2, "both notes survive: {:?}", snap.notes);
            let foot = snap
                .notes
                .iter()
                .find(|n| n.id == f_id)
                .expect("footnote note present by id");
            assert_eq!(foot.kind, "footnote");
            assert_eq!(foot.blocks[0].text, "a footnote");
            let end = snap
                .notes
                .iter()
                .find(|n| n.kind == "endnote")
                .expect("endnote present");
            assert_eq!(end.blocks[0].text, "an endnote");

            let exported2 = export_odt(&d2);
            let mut d3 = Document::new();
            import_odt(&exported2, &mut d3);
            let kinds_texts = |snap: &DocSnapshot| {
                let mut v: Vec<(String, String)> = snap
                    .notes
                    .iter()
                    .map(|n| {
                        (
                            n.kind.clone(),
                            n.blocks.iter().map(|b| b.text.clone()).collect(),
                        )
                    })
                    .collect();
                v.sort();
                v
            };
            assert_eq!(
                kinds_texts(&d2.materialize()),
                kinds_texts(&d3.materialize()),
                "notes stable across a second round-trip"
            );
        }
        // Phase `comment_imports_from_inline_annotation`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::MARK_COMMENT;
            // A paragraph "fix this word" with an annotation spanning "this word": the annotation sits at the range start, an annotation-end at the range end.
            let body = concat!(
                r#"<text:p>fix "#,
                r#"<office:annotation office:name="c1"><dc:creator>Ada</dc:creator><dc:date>2021-01-02T03:04:05</dc:date><text:p>note: fix</text:p></office:annotation>"#,
                r#"this word"#,
                r#"<office:annotation-end office:name="c1"/>"#,
                r#"</text:p>"#,
            );
            let odt = minimal_odt(body);
            let mut d = Document::new();
            import_odt(&odt, &mut d);
            let snap = d.materialize();

            let p = &snap.blocks[0];
            assert_eq!(
                p.text, "fix this word",
                "annotation markup not in text: {:?}",
                p.text
            );
            let cm = p
                .marks
                .iter()
                .find(|m| m.name == MARK_COMMENT)
                .expect("comment mark present");
            assert_eq!(
                (cm.start, cm.end),
                (4, 13),
                "marker spans 'this word': {:?}",
                p.marks
            );
            let cid = match &cm.value {
                MarkValue::Str(s) => s.clone(),
                other => panic!("comment value is the comment id, got {other:?}"),
            };
            assert_eq!(snap.comments.len(), 1, "one comment: {:?}", snap.comments);
            let comment = &snap.comments[0];
            assert_eq!(comment.id, cid, "marker references the comment by id");
            assert_eq!(comment.author, "Ada");
            assert_eq!(comment.date, "2021-01-02T03:04:05");
            assert_eq!(comment.blocks[0].text, "note: fix");
        }
        // Phase `comment_roundtrips_through_odt`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::MARK_COMMENT;
            // (author, body text, (range start, range end)) per comment, for a stable equality check.
            type CommentSummary = (String, String, Option<(usize, usize)>);
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "please fix this word now");
            let cid = d.add_comment(&p, 11, 20, "Ada Lovelace", "note: fix"); // over "this word"
            let _ = cid;

            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert!(
                content.contains("<office:annotation"),
                "annotation emitted: {content}"
            );
            assert!(
                content.contains("<office:annotation-end"),
                "annotation-end emitted (ranged): {content}"
            );
            assert!(
                content.contains("<dc:creator>Ada Lovelace</dc:creator>"),
                "author emitted: {content}"
            );
            assert!(
                content.contains("note: fix"),
                "comment body emitted: {content}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            assert_eq!(snap.blocks[0].text, "please fix this word now");
            let cm = snap.blocks[0]
                .marks
                .iter()
                .find(|m| m.name == MARK_COMMENT)
                .expect("comment marker survives");
            assert_eq!(
                (cm.start, cm.end),
                (11, 20),
                "anchored range survives: {:?}",
                snap.blocks[0].marks
            );
            let new_id = match &cm.value {
                MarkValue::Str(s) => s.clone(),
                _ => panic!("comment value"),
            };
            assert_eq!(
                snap.comments.len(),
                1,
                "comment survives: {:?}",
                snap.comments
            );
            let cmt = snap
                .comments
                .iter()
                .find(|c| c.id == new_id)
                .expect("comment present by id");
            assert_eq!(cmt.author, "Ada Lovelace");
            assert_eq!(cmt.blocks[0].text, "note: fix");
            assert_eq!(cmt.range, Some((snap.blocks[0].id.clone(), 11, 20)));

            let exported2 = export_odt(&d2);
            let mut d3 = Document::new();
            import_odt(&exported2, &mut d3);
            let authors_texts = |snap: &DocSnapshot| {
                let mut v: Vec<CommentSummary> = snap
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
        // Phase `tracked_changes_roundtrip_through_odt`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::{MARK_DEL, MARK_INS};
            // A tracked insertion + a tracked deletion (with author) must export as a text:tracked-changes
            // block + inline change-start/change-end markers, and re-import with kind/author + ranges intact.
            let d = build_tracked_changes_doc();

            let exported = export_odt(&d);
            let cont = OdtContainer::read(&exported).unwrap();
            let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
            assert_tracked_changes_xml(content);

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let snap = d2.materialize();
            assert_eq!(
                snap.blocks[0].text, "hello there world",
                "ins kept, del retained: {:?}",
                snap.blocks[0].text
            );
            let im = snap.blocks[0]
                .marks
                .iter()
                .find(|m| m.name == MARK_INS)
                .expect("ins mark survives");
            assert_eq!(
                (im.start, im.end),
                (5, 11),
                "ins range survives: {:?}",
                snap.blocks[0].marks
            );
            let dm = snap.blocks[0]
                .marks
                .iter()
                .find(|m| m.name == MARK_DEL)
                .expect("del mark survives");
            assert_eq!(
                (dm.start, dm.end),
                (12, 17),
                "del range survives: {:?}",
                snap.blocks[0].marks
            );
            let ins_id = match &im.value {
                MarkValue::Str(s) => s.clone(),
                _ => panic!("ins value"),
            };
            let del_id = match &dm.value {
                MarkValue::Str(s) => s.clone(),
                _ => panic!("del value"),
            };
            assert_eq!(
                snap.changes.len(),
                2,
                "both changes survive: {:?}",
                snap.changes
            );
            let ic = snap
                .changes
                .iter()
                .find(|c| c.id == ins_id)
                .expect("ins change present");
            assert_eq!(ic.kind, tilo_doc::snapshot::ChangeKind::Ins);
            assert_eq!(ic.author, "Ada");
            assert_eq!(ic.range, Some((snap.blocks[0].id.clone(), 5, 11)));
            let dc = snap
                .changes
                .iter()
                .find(|c| c.id == del_id)
                .expect("del change present");
            assert_eq!(dc.kind, tilo_doc::snapshot::ChangeKind::Del);
            assert_eq!(dc.author, "Bob");
            assert_eq!(dc.range, Some((snap.blocks[0].id.clone(), 12, 17)));

            // Accept on the imported document: insertion clears its mark (text kept), deletion removes its struck text.
            d2.accept_change(&ins_id);
            d2.accept_change(&del_id);
            let after = d2.materialize();
            assert_eq!(
                after.blocks[0].text, "hello there ",
                "accept-ins keeps text; accept-del removes 'world'"
            );
            assert!(after.changes.is_empty(), "both changes resolved");
        }
    }

    /// Builds a paragraph "hello world" with a tracked insertion (" there" after "hello", by
    /// Ada) and a tracked deletion ("world", by Bob), for the tracked-changes round-trip test.
    fn build_tracked_changes_doc() -> Document {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        // Insert " there" after "hello" (idx 5).
        let ins = d.track_insert(&p, 5, " there", "Ada", "2021-01-02T00:00:00");
        // Delete "world" (now chars 12..17 in "hello there world").
        let del = d.track_delete(&p, 12, 17, "Bob", "2021-03-04T00:00:00");
        let _ = (ins, del);
        d
    }

    /// Asserts the exported ODT `content.xml` carries the tracked-changes block, both region
    /// wrappers, both inline change markers, and both authors.
    fn assert_tracked_changes_xml(content: &str) {
        assert!(
            content.contains("<text:tracked-changes>"),
            "tracked-changes block emitted: {content}"
        );
        assert!(
            content.contains("<text:insertion>"),
            "insertion region emitted: {content}"
        );
        assert!(
            content.contains("<text:deletion>"),
            "deletion region emitted: {content}"
        );
        assert!(
            content.contains("<text:change-start"),
            "change-start marker emitted: {content}"
        );
        assert!(
            content.contains("<text:change-end"),
            "change-end marker emitted: {content}"
        );
        assert!(
            content.contains("<dc:creator>Ada</dc:creator>"),
            "ins author emitted: {content}"
        );
        assert!(
            content.contains("<dc:creator>Bob</dc:creator>"),
            "del author emitted: {content}"
        );
    }

    #[test]
    fn section_columns_roundtrip_through_odt() {
        // Phase `columns_roundtrip_synthesised_styles`.
        {
            // A freshly-authored 2-column document (no styles.xml) must synthesise one carrying the page-layout `style:columns`, and re-import the count + gap.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "flows across columns");
            d.set_columns(2, Some(48.0)); // 48 px ≈ 1.27 cm

            let exported = export_odt(&d);
            let out = OdtContainer::read(&exported).unwrap();
            let styles =
                std::str::from_utf8(out.get("styles.xml").expect("styles.xml synthesised"))
                    .unwrap();
            assert!(
                styles.contains(r#"style:column-count="2""#),
                "column-count in page-layout: {styles}"
            );
            assert!(
                styles.contains("style:column-gap="),
                "column-gap emitted: {styles}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            let (count, gap) = d2.columns();
            assert_eq!(count, 2, "column count survives .odt round-trip");
            // The gap round-trips through px → cm → px; allow a small unit-conversion epsilon.
            let gap = gap.expect("gap present");
            assert!(
                (gap - 48.0).abs() < 0.5,
                "column gap ≈ 48 px after round-trip, got {gap}"
            );
            assert_eq!(
                d2.materialize().columns,
                2,
                "snapshot surfaces the column count"
            );
        }
        // Phase `columns_preserve_existing_page_layout`.
        {
            // An imported styles.xml whose page-layout carries page-size/margins must keep them when `style:columns` is injected — only the columns change.
            let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"><office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties fo:page-width="21cm" fo:margin-left="2cm"/></style:page-layout></office:automatic-styles></office:document-styles>"#;
            let base = minimal_odt_styled("", "<text:p>hi</text:p>");
            let mut c = OdtContainer::read(&base).unwrap();
            c.set("styles.xml", styles_xml.as_bytes().to_vec());
            let odt = c.write().unwrap();

            let mut d = Document::new();
            import_odt(&odt, &mut d);
            assert_eq!(d.columns(), (1, None), "no columns initially");
            d.set_columns(3, None);

            let exported = export_odt(&d);
            let out = OdtContainer::read(&exported).unwrap();
            let styles_out = std::str::from_utf8(out.get("styles.xml").unwrap()).unwrap();
            assert!(
                styles_out.contains(r#"fo:page-width="21cm""#),
                "page size preserved: {styles_out}"
            );
            assert!(
                styles_out.contains(r#"fo:margin-left="2cm""#),
                "margin preserved: {styles_out}"
            );
            assert!(
                styles_out.contains(r#"style:column-count="3""#),
                "columns injected: {styles_out}"
            );

            let mut d2 = Document::new();
            import_odt(&exported, &mut d2);
            assert_eq!(
                d2.columns().0,
                3,
                "column count survives alongside the page-layout"
            );
        }
        // Phase `columns_replace_existing_style_columns`.
        {
            // A page-layout that already declares `style:columns` must have it replaced (not duplicated) by the document's setting.
            let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"><office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties><style:columns style:column-count="4"/></style:page-layout-properties></style:page-layout></office:automatic-styles></office:document-styles>"#;
            let base = minimal_odt_styled("", "<text:p>hi</text:p>");
            let mut c = OdtContainer::read(&base).unwrap();
            c.set("styles.xml", styles_xml.as_bytes().to_vec());
            let odt = c.write().unwrap();

            let mut d = Document::new();
            import_odt(&odt, &mut d);
            assert_eq!(d.columns().0, 4, "imported 4 columns");
            d.set_columns(2, None);

            let exported = export_odt(&d);
            let out = OdtContainer::read(&exported).unwrap();
            let styles_out = std::str::from_utf8(out.get("styles.xml").unwrap()).unwrap();
            assert_eq!(
                styles_out.matches("<style:columns").count(),
                1,
                "exactly one style:columns: {styles_out}"
            );
            assert!(
                styles_out.contains(r#"style:column-count="2""#),
                "count updated to 2: {styles_out}"
            );
        }
    }

    /// Build a `.odt` whose preserved `styles.xml` holds two independent top-level style atoms — the canonical "two unmodeled regions in one part" shape.
    fn odt_with_two_style_atoms() -> Vec<u8> {
        let styles_xml = concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            "\n",
            r#"<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"><office:styles>"#,
            r#"<style:style style:name="Quote" style:family="paragraph"><style:text-properties style:font-name="QuoteFont"/></style:style>"#,
            r#"<style:style style:name="Caption" style:family="paragraph"><style:text-properties style:font-name="CaptionFont"/></style:style>"#,
            r#"</office:styles></office:document-styles>"#,
        );
        let base = minimal_odt_styled("", "<text:p>hi</text:p>");
        let mut c = OdtContainer::read(&base).unwrap();
        c.set("styles.xml", styles_xml.as_bytes().to_vec());
        c.write().unwrap()
    }

    /// The id + current bytes of the preserved `styles.xml` atom containing `needle`.
    fn atom_containing(d: &Document, needle: &str) -> (String, Vec<u8>) {
        let r = d.as_ref();
        tilo_doc::model::preserved_atoms(r.doc, r.c, "odt/part/styles.xml")
            .into_iter()
            .find(|(_, b)| String::from_utf8_lossy(b).contains(needle))
            .unwrap_or_else(|| panic!("no preserved styles.xml atom contains {needle:?}"))
    }

    #[test]
    fn unedited_page_regions_keep_their_original_elements() {
        let styles = concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">"#,
            r#"<office:master-styles><style:master-page style:name="Standard">"#,
            r#"<style:header><text:p>Head</text:p></style:header>"#,
            r#"<style:header-first><text:p>Cover</text:p></style:header-first>"#,
            r#"<style:footer><text:p>Page <text:page-number>1</text:page-number> of <text:page-count>2</text:page-count></text:p></style:footer>"#,
            r#"</style:master-page></office:master-styles></office:document-styles>"#,
        );
        let mut c = OdtContainer::read(&minimal_odt("<text:p>body</text:p>")).unwrap();
        c.set("styles.xml", styles.as_bytes().to_vec());
        let odt = c.write().unwrap();
        let styles_of = |bytes: &[u8]| {
            let c = OdtContainer::read(bytes).unwrap();
            String::from_utf8(c.get("styles.xml").unwrap().to_vec()).unwrap()
        };

        let mut d = Document::new();
        OdtCodec
            .import(
                &odt,
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        assert_eq!(
            styles_of(
                &OdtCodec
                    .export(d.as_ref(), &tilo_content::NoContent)
                    .unwrap()
            ),
            styles
        );

        // Editing the header regenerates it (dropping the first-page variant with it); the
        // unedited footer keeps its page count.
        let head = d.header_blocks_in_order()[0].clone();
        d.splice_header_text(&head, 4, 0, "er");
        let out = styles_of(
            &OdtCodec
                .export(d.as_ref(), &tilo_content::NoContent)
                .unwrap(),
        );
        assert!(out.contains("<text:p>Header</text:p>"), "{out}");
        assert!(
            out.contains("<text:page-count>2</text:page-count>"),
            "{out}"
        );
    }

    #[test]
    fn keeps_paragraph_span_and_table_styles_while_they_agree() {
        let styles = concat!(
            r#"<style:style style:name="P1" style:family="paragraph"><style:paragraph-properties fo:break-before="page" fo:text-align="center"/></style:style>"#,
            r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:letter-spacing="0.1cm" fo:font-weight="bold"/></style:style>"#,
            r#"<style:style style:name="Tbl" style:family="table"><style:table-properties style:width="10cm"/></style:style>"#,
            r#"<style:style style:name="Col" style:family="table-column"><style:table-column-properties style:column-width="5cm"/></style:style>"#,
            r#"<style:style style:name="Cell" style:family="table-cell"><style:table-cell-properties fo:border="0.5pt solid #000000"/></style:style>"#,
        );
        let body = concat!(
            r#"<text:p text:style-name="P1">A <text:span text:style-name="T1">spaced</text:span> word</text:p>"#,
            r#"<text:p text:style-name="P1">second</text:p>"#,
            r#"<table:table table:name="T" table:style-name="Tbl"><table:table-column table:style-name="Col" table:number-columns-repeated="2"/>"#,
            r#"<table:table-row><table:table-cell table:style-name="Cell"><text:p>a</text:p></table:table-cell>"#,
            r#"<table:table-cell table:style-name="Cell"><text:p>b</text:p></table:table-cell></table:table-row></table:table>"#,
        );
        let mut d = Document::new();
        OdtCodec
            .import(
                &minimal_odt_styled(styles, body),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let snap = d.materialize();
        assert!(
            snap.blocks[0]
                .marks
                .iter()
                .any(|m| m.name == "strong" && m.start == 2 && m.end == 8)
        );
        // The second paragraph is realigned in the model: its style no longer agrees.
        let second = snap.blocks[1].id.clone();
        d.as_mut().set_attr(&second, "align", "right");

        let exported = OdtCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        for kept in [
            r#"<text:p text:style-name="P1">A <text:span text:style-name="T1"><text:span text:style-name="qdoc-strong">spaced</text:span></text:span>"#,
            r#"<table:table table:name="T" table:style-name="Tbl"><table:table-column table:style-name="Col" table:number-columns-repeated="2"/>"#,
            r#"<table:table-cell table:style-name="Cell"><text:p>a</text:p>"#,
            r#"<style:style style:name="P1" style:family="paragraph">"#,
        ] {
            assert!(content.contains(kept), "{kept} in {content}");
        }
        assert!(
            !content.contains(r#"<text:p text:style-name="P1">second"#),
            "{content}"
        );
        let mut d2 = Document::new();
        OdtCodec
            .import(
                &exported,
                &mut d2.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let again = d2.materialize();
        assert_eq!(norm(&again)[0], norm(&snap)[0]);
        assert_eq!(
            again.blocks[1].attrs.get("align"),
            Some(&AttrVal::Str("right".into()))
        );
    }

    #[test]
    fn deleted_text_moves_between_body_and_changed_region() {
        let body = concat!(
            r#"<text:tracked-changes><text:changed-region xml:id="ct2" text:id="ct2"><text:deletion>"#,
            r#"<office:change-info><dc:creator>Bob</dc:creator></office:change-info><text:p>removed words</text:p>"#,
            r#"</text:deletion></text:changed-region></text:tracked-changes>"#,
            r#"<text:p>Tracked: <text:change text:change-id="ct2"/>gone.</text:p>"#,
        );
        let mut d = Document::new();
        OdtCodec
            .import(
                &minimal_odt(body),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let snap = d.materialize();
        let p = &snap.blocks[0];
        assert_eq!(p.text, "Tracked: removed wordsgone.");
        assert!(
            p.marks
                .iter()
                .any(|m| m.name == "del" && m.start == 9 && m.end == 22),
            "{:?}",
            p.marks
        );

        let exported = OdtCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(
            content.contains("<text:p>removed words</text:p></text:deletion>"),
            "{content}"
        );
        assert!(
            content.contains(r#"Tracked: <text:change text:change-id=""#),
            "{content}"
        );
        assert!(!content.contains("removed wordsgone"), "{content}");
        let mut d2 = Document::new();
        OdtCodec
            .import(
                &exported,
                &mut d2.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        assert_eq!(norm(&d2.materialize()), norm(&snap));
    }

    #[test]
    fn merged_cells_roundtrip_through_odt() {
        let body = concat!(
            r#"<table:table table:name="T"><table:table-column table:number-columns-repeated="3"/>"#,
            r#"<table:table-row><table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"><text:p>big</text:p></table:table-cell>"#,
            r#"<table:covered-table-cell/><table:table-cell><text:p>c</text:p></table:table-cell></table:table-row>"#,
            r#"<table:table-row><table:covered-table-cell table:number-columns-repeated="2"/><table:table-cell><text:p>f</text:p></table:table-cell></table:table-row>"#,
            r#"</table:table>"#,
        );
        let mut d = Document::new();
        OdtCodec
            .import(
                &minimal_odt(body),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let snap = d.materialize();
        let big = snap.blocks.iter().find(|b| b.text == "big").unwrap();
        assert_eq!(big.attrs.get("colspan"), Some(&AttrVal::Int(2)));
        assert_eq!(big.attrs.get("rowspan"), Some(&AttrVal::Int(2)));
        let f = snap.blocks.iter().find(|b| b.text == "f").unwrap();
        assert_eq!(f.attrs.get("col"), Some(&AttrVal::Int(2)), "{:?}", f.attrs);

        let exported = OdtCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert!(
            content.contains(r#"<table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"><text:p>big</text:p></table:table-cell><table:covered-table-cell/>"#),
            "{content}"
        );
        assert_eq!(content.matches("<table:covered-table-cell/>").count(), 3);
        let mut d2 = Document::new();
        OdtCodec
            .import(
                &exported,
                &mut d2.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        assert_eq!(norm(&d2.materialize()), norm(&snap));
    }

    #[test]
    fn numbered_then_bulleted_lists_stay_apart_and_numbered() {
        let mut d = Document::new();
        let one = d.insert_block(None, "list_item");
        d.splice_text(&one, 0, 0, "one");
        d.as_mut().set_attr(&one, "ordered", true);
        let bullet = d.insert_block(Some(&one), "list_item");
        d.splice_text(&bullet, 0, 0, "dot");
        d.as_mut().set_attr(&bullet, "ordered", false);

        let exported = OdtCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        let cont = OdtContainer::read(&exported).unwrap();
        let content = std::str::from_utf8(cont.get("content.xml").unwrap()).unwrap();
        assert_eq!(content.matches("<text:list ").count(), 2, "{content}");

        let mut d2 = Document::new();
        OdtCodec
            .import(
                &exported,
                &mut d2.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let snap = d2.materialize();
        assert_eq!(
            snap.blocks[0].attrs.get("ordered"),
            Some(&AttrVal::Bool(true))
        );
        assert_eq!(
            snap.blocks[1].attrs.get("ordered"),
            Some(&AttrVal::Bool(false))
        );
    }
}
