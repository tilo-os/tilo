//! Export the canonical model back to a `.odt`: generate a fresh minimal ODF body from
//! the model, then splice it into the original container's preserved parts (styles.xml,
//! images, meta.xml, manifest, …) so unmodeled content survives verbatim.

use crate::format::CodecError;
use crate::package::ZipPackage;
use crate::preserve::MARK_ODT_XML;
use crate::resolved::Resolved;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use tilo_content::ContentSource;
use tilo_doc::document::MarkSpan;
use tilo_doc::export::{CellGrid, ExportBlock, InlinePlan};
use tilo_doc::model::DocRef;
use tilo_doc::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME,
    STYLE_SIZE, STYLE_SPACE_AFTER, STYLE_SPACE_BEFORE,
};
use tilo_doc::snapshot::{AttrVal, DocSnapshot};

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

    let mut c = ZipPackage { parts: Vec::new() };
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
    if let Some(meta_xml) = super::meta::export_meta(&doc, original_meta.as_deref()) {
        let is_new = !had_meta;
        c.set("meta.xml", meta_xml);
        if is_new {
            had_meta = true;
        }
    }
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
    c.write_odf()
}

/// Splice header/footer, multi-column layout, and default-language into `styles.xml`
/// (synthesising a minimal one if none was preserved). Sets `had_styles` the first time
/// anything is actually written, so the caller knows whether to add a manifest entry for it.
fn splice_styles_xml(
    doc: &DocRef,
    c: &mut ZipPackage,
    original_styles: Option<&[u8]>,
    had_styles: &mut bool,
) {
    if let Some(styles_xml) = super::headers::export_master_headers(doc, original_styles) {
        let is_new = !*had_styles;
        c.set("styles.xml", styles_xml);
        if is_new {
            *had_styles = true;
        }
    }
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

fn write_image_blobs(
    snap: &Resolved<'_>,
    c: &mut ZipPackage,
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
            let (ext, media) = crate::office_image_part(bytes);
            let path = format!("Pictures/{hash}.{ext}");
            c.set(&path, bytes.to_vec());
            if !pic_entries.iter().any(|(p, _)| p == &path) {
                pic_entries.push((path, media));
            }
        }
    }
    pic_entries
}

fn picture_href(hash: &str, bytes: &[u8]) -> String {
    let (ext, _) = crate::office_image_part(bytes);
    format!("Pictures/{hash}.{ext}")
}

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
        "super" => Some((
            "qdoc-super".into(),
            r#"style:text-position="super 58%""#.into(),
        )),
        "sub" => Some(("qdoc-sub".into(), r#"style:text-position="sub 58%""#.into())),
        "lang" => m
            .value
            .as_str()
            .filter(|c| !c.is_empty())
            .map(|c| (format!("qdoc-lang-{}", lang_token(c)), lang_text_props(c))),
        _ => None,
    }
}

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

type CommentIndex<'a> = std::collections::BTreeMap<String, &'a tilo_doc::snapshot::CommentView>;

fn comment_index(snap: &DocSnapshot) -> CommentIndex<'_> {
    snap.comments.iter().map(|c| (c.id.clone(), c)).collect()
}

struct InlineCtx<'a> {
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
    let anchor = ExportBlock::new(b).image_anchor();
    let wrap = ExportBlock::new(b).image_wrap();
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
        let item = ExportBlock::new(li);
        let ordered = item.ordered();
        Self {
            ordered,
            format: if ordered {
                item.num_format().to_string()
            } else {
                String::new()
            },
            start: if ordered { item.num_start() } else { 1 },
        }
    }
}

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
    if start != 1 && ExportBlock::new(li).num_restart() {
        let _ = write!(tag, r#" text:start-value="{start}""#);
    } else if start != 1 {
        tag.push_str(r#" text:continue-numbering="true""#);
    }
    tag.push('>');
    tag
}

type ParaStyleEntry = (Option<String>, String, String);

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
            && self.registry.table.para_align(style) == align
            && self.registry.table.para_line_height(style) == line;
        agrees.then_some(style.as_str())
    }

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
                if let Some(frame) = image_frame_xml(b, snap, &mut graphic_styles) {
                    body.push_str(&frame);
                }
                i = i.saturating_add(1);
            }
            "fidelity" => {
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
    let level = ExportBlock::new(b).heading_level();
    let sa = style_attr(b);
    let _ = write!(
        body,
        r#"<text:h text:outline-level="{level}"{sa}>{}</text:h>"#,
        render_inline(&b.text, &b.marks, inline)
    );
}

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
        let target = ExportBlock::new(li).list_level().saturating_add(1); // lists that should be open
        let key = ListKey::of(li);
        // A top-level item of another kind (numbered after bulleted, another format) starts a new list.
        if target == 1 && depth > 0 && top.as_ref() != Some(&key) {
            close_lists(body, &mut depth, &mut item_open);
        }
        if depth == 0 {
            top = Some(key.clone());
        }
        if target > depth {
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

fn render_table_run(
    snap: &Resolved<'_>,
    i: &mut usize,
    body: &mut String,
    style_attr: &mut impl FnMut(&BlockView) -> String,
    inline: &mut InlineCtx<'_>,
) {
    let grid = CellGrid::run(&snap.blocks, *i);
    let cells = grid.cells();
    *i = i.saturating_add(cells.len().max(1));
    let ncols = grid.cols();
    push_table_open(body, grid.table(), cells.first(), ncols);
    let mut row_styles: BTreeMap<usize, &str> = BTreeMap::new();
    for cell in cells {
        if let Some(AttrVal::Str(s)) = cell.attrs.get(super::content::ODT_ROW_STYLE) {
            row_styles
                .entry(ExportBlock::new(cell).cell_position().0)
                .or_insert(s.as_str());
        }
    }
    for r in 0..grid.rows() {
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
            if grid.covering(r, c).is_some() {
                body.push_str("<table:covered-table-cell/>");
                continue;
            }
            let cell = grid.origin(r, c);
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
    let kept = |key: &str| first.and_then(|c| c.str_attr(key));
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

fn cell_spans_xml(b: &BlockView) -> String {
    let mut out = String::new();
    let (cols, rows) = (
        ExportBlock::new(b).col_span(),
        ExportBlock::new(b).row_span(),
    );
    if cols > 1 {
        let _ = write!(out, r#" table:number-columns-spanned="{cols}""#);
    }
    if rows > 1 {
        let _ = write!(out, r#" table:number-rows-spanned="{rows}""#);
    }
    out
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
    use super::super::container::{minimal_odt, minimal_odt_styled};
    use crate::FormatProvider;
    use crate::package::ZipPackage;
    use std::collections::BTreeMap;
    use tilo_doc::document::MarkValue;
    use tilo_doc::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_COMMENT, MARK_NOTE_REF};
    use tilo_doc::{AttrVal, DocSnapshot, Document};

    fn import_odt(bytes: &[u8], doc: &mut Document) {
        OdtCodec
            .import(
                bytes,
                &mut doc.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
    }

    fn export_odt(doc: &Document) -> Vec<u8> {
        OdtCodec
            .export(doc.as_ref(), &tilo_content::NoContent)
            .unwrap()
    }

    fn part(bytes: &[u8], name: &str) -> String {
        String::from_utf8(ZipPackage::read(bytes).unwrap().get(name).unwrap().to_vec()).unwrap()
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

    type NormalizedRegionBlock = (
        String,
        Vec<(usize, usize, String, tilo_doc::document::MarkValue)>,
    );

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

    /// Assert every `(name, start, end, value)` mark is on the block whose text is `text`.
    fn assert_marks(snap: &DocSnapshot, text: &str, want: &[(usize, usize, &str, MarkValue)]) {
        let b = snap
            .blocks
            .iter()
            .find(|b| b.text == text)
            .unwrap_or_else(|| panic!("no block {text:?} in {snap:?}"));
        for (start, end, name, value) in want {
            assert!(
                b.marks.iter().any(|m| m.name == *name
                    && m.start == *start
                    && m.end == *end
                    && m.value == *value),
                "{text:?} lacks {name} {start}..{end} = {value:?}: {:?}",
                b.marks
            );
        }
    }

    const TINY_PNG: [u8; 69] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xf7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn block<'a>(snap: &'a DocSnapshot, text: &str) -> &'a tilo_doc::snapshot::BlockView {
        snap.blocks
            .iter()
            .find(|b| b.text == text)
            .unwrap_or_else(|| panic!("no block {text:?} in {snap:?}"))
    }

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
        let mut c = ZipPackage::read(&base).unwrap();
        c.set("styles.xml", styles_xml.as_bytes().to_vec());
        c.write_odf().unwrap()
    }

    fn atom_containing(d: &Document, needle: &str) -> (String, Vec<u8>) {
        let r = d.as_ref();
        tilo_doc::model::preserved_atoms(r.doc, r.c, "odt/part/styles.xml")
            .into_iter()
            .find(|(_, b)| String::from_utf8_lossy(b).contains(needle))
            .unwrap_or_else(|| panic!("no preserved styles.xml atom contains {needle:?}"))
    }

    /// A document a person wrote in Tilo — styled text, a named style, links, a bookmark and its
    /// cross-reference, two images, nested and numbered lists, a footnote, an endnote, a comment,
    /// tracked changes, a header and a footer with the page number, two columns and document
    /// properties — is saved as `.odt`, reads back as the same model, and carries the ODF that
    /// `LibreOffice` and Word read for each of them.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one save and reopen of a single document is one flow"
    )]
    fn a_document_of_everything_saves_and_reopens_as_odt() {
        let mut d = Document::new();

        // Text and paragraph formatting.
        let sup = d.insert_block(None, "paragraph");
        d.splice_text(&sup, 0, 0, "E=mc2");
        {
            let mut dm = d.as_mut();
            dm.add_mark(&sup, 0, 5, "highlight", "#ffff00");
            dm.add_mark(&sup, 4, 5, "super", true);
            dm.set_attr(&sup, "line_spacing", 1.5_f64);
        }
        let sub = d.insert_block(Some(&sup), "paragraph");
        d.splice_text(&sub, 0, 0, "H2O");
        d.as_mut().add_mark(&sub, 1, 2, "sub", true);
        let red = d.insert_block(Some(&sub), "paragraph");
        d.splice_text(&red, 0, 0, "red text");
        d.as_mut().add_mark(&red, 0, 3, "color", "#ff0000");
        let styled = d.insert_block(Some(&red), "paragraph");
        d.splice_text(&styled, 0, 0, "styled");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&styled, "align", "center");
            dm.add_mark(&styled, 0, 6, "fontSize", 22_i64);
            dm.add_mark(&styled, 0, 6, "fontFamily", "serif");
        }
        let justified = d.insert_block(Some(&styled), "paragraph");
        d.splice_text(&justified, 0, 0, "justified");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&justified, "align", "justify");
            dm.add_mark(&justified, 0, 9, "color", "#3366cc");
            dm.add_mark(&justified, 0, 9, "fontFamily", "serif");
            dm.add_mark(&justified, 0, 9, "fontSize", 20_i64);
        }
        let fancy = d.insert_block(Some(&justified), "paragraph");
        d.splice_text(&fancy, 0, 0, "fancy");
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
            dm.set_attr(&fancy, "style", "Fancy");
        }

        // Links, a bookmark and a cross-reference to it.
        let home = d.insert_block(Some(&fancy), "paragraph");
        d.splice_text(&home, 0, 0, "go home now");
        d.as_mut().add_mark(&home, 3, 7, "link", "https://tilo.so/");
        let amp = d.insert_block(Some(&home), "paragraph");
        d.splice_text(&amp, 0, 0, "link");
        d.as_mut()
            .add_mark(&amp, 0, 4, "link", "https://x/?a=1&b=2");
        let click = d.insert_block(Some(&amp), "paragraph");
        d.splice_text(&click, 0, 0, "click");
        {
            let mut dm = d.as_mut();
            dm.add_mark(&click, 0, 5, "link", "http://q/");
            dm.add_mark(&click, 0, 5, "strong", true);
            dm.add_mark(&click, 0, 5, "color", "#0000ff");
        }
        let target = d.insert_block(Some(&click), "paragraph");
        d.splice_text(&target, 0, 0, "the intro section");
        d.add_bookmark(&target, 4, 9, "intro");
        let referrer = d.insert_block(Some(&target), "paragraph");
        d.splice_text(&referrer, 0, 0, "see above");
        d.add_cross_ref(&referrer, 4, 9, "intro");

        // Two images: one plain with alt text, one anchored and wrapped.
        let mut inputs = tilo_content::ContentInputs::new();
        let before = d.insert_block(Some(&referrer), "paragraph");
        d.splice_text(&before, 0, 0, "before");
        let image = d.insert_block(Some(&before), "image");
        let hash = d.attach(&TINY_PNG, &mut inputs);
        {
            let mut dm = d.as_mut();
            dm.set_attr(&image, "blob", hash.as_str());
            dm.set_attr(&image, "alt", "a tiny red dot");
            dm.set_attr(&image, "width", 64_i64);
            dm.set_attr(&image, "height", 48_i64);
        }
        let after = d.insert_block(Some(&image), "paragraph");
        d.splice_text(&after, 0, 0, "after");
        let wrapped = d.insert_block(Some(&after), "image");
        {
            let mut dm = d.as_mut();
            dm.set_attr(&wrapped, "blob", hash.as_str());
            dm.set_attr(&wrapped, "anchor", "paragraph");
            dm.set_attr(&wrapped, "wrap", "right");
            dm.set_attr(&wrapped, "width", 120_i64);
            dm.set_attr(&wrapped, "height", 80_i64);
        }

        // Nested lists, an upper-roman list starting at 3, and a bulleted list after a numbered one.
        let item_a = d.insert_block(Some(&wrapped), "list_item");
        d.splice_text(&item_a, 0, 0, "a");
        let item_b = d.insert_block(Some(&item_a), "list_item");
        d.splice_text(&item_b, 0, 0, "b");
        let nested = d.insert_block(Some(&item_b), "list_item");
        d.splice_text(&nested, 0, 0, "nested");
        d.as_mut().set_attr(&nested, "level", 1_i64);
        let item_c = d.insert_block(Some(&nested), "list_item");
        d.splice_text(&item_c, 0, 0, "c");
        let roman = d.insert_block(Some(&item_c), "list_item");
        d.splice_text(&roman, 0, 0, "three");
        let roman2 = d.insert_block(Some(&roman), "list_item");
        d.splice_text(&roman2, 0, 0, "four");
        {
            let mut dm = d.as_mut();
            for id in [&roman, &roman2] {
                dm.set_attr(id, "ordered", true);
                dm.set_attr(id, "num_format", "upper-roman");
                dm.set_attr(id, "num_start", 3_i64);
                dm.set_attr(id, "num_restart", true);
            }
        }
        let numbered = d.insert_block(Some(&roman2), "list_item");
        d.splice_text(&numbered, 0, 0, "one");
        d.as_mut().set_attr(&numbered, "ordered", true);
        let bullet = d.insert_block(Some(&numbered), "list_item");
        d.splice_text(&bullet, 0, 0, "dot");
        d.as_mut().set_attr(&bullet, "ordered", false);

        // A footnote and an endnote.
        let noted = d.insert_block(Some(&bullet), "paragraph");
        d.splice_text(&noted, 0, 0, "see ref");
        let foot = d.insert_footnote(&noted, 4);
        let foot_block = d.insert_note_block(&foot, None, "paragraph").unwrap();
        d.splice_note_text(&foot, &foot_block, 0, 0, "a footnote");
        let tail = d.insert_block(Some(&noted), "paragraph");
        d.splice_text(&tail, 0, 0, "tail");
        let end = d.insert_endnote(&tail, 4);
        let end_block = d.insert_note_block(&end, None, "paragraph").unwrap();
        d.splice_note_text(&end, &end_block, 0, 0, "an endnote");

        // A comment and tracked insert and delete.
        let commented = d.insert_block(Some(&tail), "paragraph");
        d.splice_text(&commented, 0, 0, "please fix this word now");
        let _ = d.add_comment(&commented, 11, 20, "Ada Lovelace", "note: fix");
        let tracked = d.insert_block(Some(&commented), "paragraph");
        d.splice_text(&tracked, 0, 0, "hello world");
        d.track_insert(&tracked, 5, " there", "Ada", "2021-01-02T00:00:00");
        d.track_delete(&tracked, 12, 17, "Bob", "2021-03-04T00:00:00");

        // A header, a footer with the page number, two columns and document properties.
        let header = d.insert_header_block(None, "paragraph");
        d.splice_header_text(&header, 0, 0, "Title");
        let footer = d.insert_footer_block(None, "paragraph");
        d.splice_footer_text(&footer, 0, 0, "Page ");
        d.insert_footer_page_number(&footer, 5);
        d.set_columns(2, Some(48.0));
        d.set_doc_prop("title", "My Title");
        d.set_doc_prop("author", "Ada Lovelace");
        d.set_doc_prop("subject", "Analytical Engine");
        d.set_doc_prop("keywords", "computing, history");
        d.set_doc_prop("created", "2020-01-02T03:04:05");
        d.set_doc_prop("modified", "2021-06-07T08:09:10");
        d.set_doc_prop("generator", "qdoc/1.0");

        let exported = OdtCodec.export(d.as_ref(), &inputs).unwrap();
        let package = ZipPackage::read(&exported).unwrap();
        assert_eq!(
            package.parts[0].0, "mimetype",
            "exported .odt has mimetype first"
        );
        assert!(package.get("content.xml").is_some());

        // What other editors read for each construct.
        let content = part(&exported, "content.xml");
        for expected in [
            r##"fo:background-color="#ffff00""##,
            r#"style:text-position="super"#,
            r#"fo:line-height="150%""#,
            r#"fo:text-align="justify""#,
            r#"text:style-name="Fancy""#,
            r#"<style:style style:name="Fancy" style:family="paragraph""#,
            r#"fo:font-weight="bold""#,
            r#"fo:font-size="20pt""#,
            r#"<text:a xlink:href="https://tilo.so/">home</text:a>"#,
            "a=1&amp;b=2",
            r#"<text:bookmark-start text:name="intro"/>"#,
            r#"<text:bookmark-end text:name="intro"/>"#,
            r##"<text:a xlink:href="#intro">above</text:a>"##,
            "<draw:frame",
            r#"text:anchor-type="paragraph""#,
            r#"style:family="graphic""#,
            r#"style:wrap="right""#,
            r"<text:list-item><text:p>b</text:p><text:list><text:list-item><text:p>nested</text:p></text:list-item></text:list></text:list-item>",
            r#"style:num-format="I""#,
            r#"text:start-value="3""#,
            r#"text:note-class="footnote""#,
            r#"text:note-class="endnote""#,
            "a footnote",
            "an endnote",
            "<office:annotation",
            "<office:annotation-end",
            "<dc:creator>Ada Lovelace</dc:creator>",
            "note: fix",
            "<text:tracked-changes>",
            "<text:insertion>",
            "<text:deletion>",
            "<text:change-start",
            "<text:change-end",
            "<dc:creator>Ada</dc:creator>",
            "<dc:creator>Bob</dc:creator>",
        ] {
            assert!(content.contains(expected), "{expected} in {content}");
        }
        let picture = package
            .parts
            .iter()
            .find(|(n, _)| n.starts_with("Pictures/") && n.to_ascii_lowercase().ends_with(".png"))
            .expect("Pictures/<hash>.png written");
        assert_eq!(picture.1, &TINY_PNG[..], "picture bytes written verbatim");
        assert!(
            content.contains(&format!(r#"xlink:href="{}""#, picture.0)),
            "image href matches part: {content}"
        );
        assert_eq!(
            content.matches("<text:list ").count(),
            4,
            "each list is written as its own list, so a bullet after a numbered item stays a bullet: {content}"
        );
        let manifest = part(&exported, "META-INF/manifest.xml");
        for expected in ["image/png", "meta.xml", "styles.xml"] {
            assert!(manifest.contains(expected), "{expected} in {manifest}");
        }
        assert!(manifest.contains(&picture.0), "picture in manifest");
        let styles_part = part(&exported, "styles.xml");
        for expected in [
            "<style:header>",
            "Title",
            "<style:footer>",
            "<text:page-number/>",
            r#"style:column-count="2""#,
            "style:column-gap=",
        ] {
            assert!(
                styles_part.contains(expected),
                "{expected} in {styles_part}"
            );
        }
        let meta = part(&exported, "meta.xml");
        for expected in [
            "<dc:title>My Title</dc:title>",
            "<dc:creator>Ada Lovelace</dc:creator>",
            "<dc:subject>Analytical Engine</dc:subject>",
            "<meta:keyword>computing, history</meta:keyword>",
            "<meta:creation-date>2020-01-02T03:04:05</meta:creation-date>",
            "<dc:date>2021-06-07T08:09:10</dc:date>",
            "<meta:generator>qdoc/1.0</meta:generator>",
        ] {
            assert!(meta.contains(expected), "{expected} in {meta}");
        }

        // Reading the saved file back gives the model the person wrote.
        let mut reopened = Document::new();
        let mut reimported = tilo_content::ContentInputs::new();
        OdtCodec
            .import(&exported, &mut reopened.as_mut(), &mut reimported)
            .unwrap();
        let snap = reopened.materialize();

        assert_marks(
            &snap,
            "E=mc2",
            &[
                (0, 5, "highlight", MarkValue::Str("#ffff00".into())),
                (4, 5, "super", MarkValue::Bool(true)),
            ],
        );
        assert_eq!(
            block(&snap, "E=mc2").attrs.get("line_spacing"),
            Some(&AttrVal::Float(1.5))
        );
        assert_marks(&snap, "H2O", &[(1, 2, "sub", MarkValue::Bool(true))]);
        assert_marks(
            &snap,
            "red text",
            &[(0, 3, "color", MarkValue::Str("#ff0000".into()))],
        );
        let styled_block = block(&snap, "styled");
        assert_eq!(
            styled_block.attrs.get("align"),
            Some(&AttrVal::Str("center".into()))
        );
        assert_marks(
            &snap,
            "styled",
            &[
                (0, 6, "fontSize", MarkValue::Int(22)),
                (0, 6, "fontFamily", MarkValue::Str("serif".into())),
            ],
        );
        assert_eq!(
            block(&snap, "justified").attrs.get("align"),
            Some(&AttrVal::Str("justify".into()))
        );
        assert_marks(
            &snap,
            "justified",
            &[
                (0, 9, "color", MarkValue::Str("#3366cc".into())),
                (0, 9, "fontFamily", MarkValue::Str("serif".into())),
                (0, 9, "fontSize", MarkValue::Int(20)),
            ],
        );
        let fancy_style = &block(&snap, "fancy").attrs;
        assert_eq!(
            fancy_style.get("style"),
            Some(&AttrVal::Str("Fancy".into())),
            "block keeps its style ref: {fancy_style:?}"
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

        assert_marks(
            &snap,
            "go home now",
            &[(3, 7, "link", MarkValue::Str("https://tilo.so/".into()))],
        );
        assert_marks(
            &snap,
            "link",
            &[(0, 4, "link", MarkValue::Str("https://x/?a=1&b=2".into()))],
        );
        assert_marks(
            &snap,
            "click",
            &[
                (0, 5, "link", MarkValue::Str("http://q/".into())),
                (0, 5, "strong", MarkValue::Bool(true)),
                (0, 5, "color", MarkValue::Str("#0000ff".into())),
            ],
        );

        assert_eq!(snap.bookmarks.len(), 1, "{:?}", snap.bookmarks);
        let bookmark = &snap.bookmarks[0];
        assert_eq!(bookmark.name, "intro");
        assert_eq!((bookmark.range.1, bookmark.range.2), (4, 9), "{bookmark:?}");
        assert_marks(
            &snap,
            "the intro section",
            &[(
                4,
                9,
                tilo_doc::schema::MARK_BOOKMARK,
                MarkValue::Str("intro".into()),
            )],
        );
        assert_marks(
            &snap,
            "see above",
            &[(4, 9, "link", MarkValue::Str("#intro".into()))],
        );

        let image_block = &snap
            .blocks
            .iter()
            .find(|b| b.kind == "image")
            .expect("image block present")
            .attrs;
        let blob_hash = match image_block.get("blob") {
            Some(AttrVal::Str(h)) => h.clone(),
            other => panic!("image block missing blob attr: {other:?}"),
        };
        assert_eq!(blob_hash, hash, "blake3 hash stable across round-trip");
        assert_eq!(
            image_block.get("alt"),
            Some(&AttrVal::Str("a tiny red dot".into()))
        );
        assert_eq!(image_block.get("width"), Some(&AttrVal::Int(64)));
        assert_eq!(image_block.get("height"), Some(&AttrVal::Int(48)));
        assert_eq!(
            reimported.get(&blob_hash),
            Some(&TINY_PNG[..]),
            "image bytes survive the round-trip"
        );
        let wrapped_block = &snap
            .blocks
            .iter()
            .filter(|b| b.kind == "image")
            .nth(1)
            .expect("second image block present")
            .attrs;
        assert_eq!(
            wrapped_block.get("anchor"),
            Some(&AttrVal::Str("paragraph".into()))
        );
        assert_eq!(
            wrapped_block.get("wrap"),
            Some(&AttrVal::Str("right".into()))
        );
        assert_eq!(wrapped_block.get("width"), Some(&AttrVal::Int(120)));
        assert_eq!(wrapped_block.get("height"), Some(&AttrVal::Int(80)));

        let level = |b: &tilo_doc::snapshot::BlockView| match b.attrs.get("level") {
            Some(AttrVal::Int(n)) => *n,
            _ => 0i64,
        };
        let items: Vec<_> = snap
            .blocks
            .iter()
            .filter(|b| ["a", "b", "nested", "c"].contains(&b.text.as_str()))
            .map(|b| (b.text.clone(), level(b)))
            .collect();
        assert_eq!(
            items,
            vec![
                ("a".to_string(), 0),
                ("b".to_string(), 0),
                ("nested".to_string(), 1),
                ("c".to_string(), 0)
            ],
            "levels round-trip through nested lists"
        );
        let roman_items: Vec<_> = snap
            .blocks
            .iter()
            .filter(|b| b.text == "three" || b.text == "four")
            .collect();
        for item in &roman_items {
            assert_eq!(
                item.attrs.get("ordered"),
                Some(&AttrVal::Bool(true)),
                "still ordered: {:?}",
                item.attrs
            );
            assert_eq!(
                item.attrs.get("num_format"),
                Some(&AttrVal::Str("upper-roman".into())),
                "format survives: {:?}",
                item.attrs
            );
            assert_eq!(
                item.attrs.get("num_start"),
                Some(&AttrVal::Int(3)),
                "start survives: {:?}",
                item.attrs
            );
        }
        assert_eq!(
            block(&snap, "one").attrs.get("ordered"),
            Some(&AttrVal::Bool(true))
        );
        assert_eq!(
            block(&snap, "dot").attrs.get("ordered"),
            Some(&AttrVal::Bool(false)),
            "a bullet after a numbered item stays a bullet"
        );

        let noted = block(&snap, &format!("see {FIELD_PLACEHOLDER}ref"));
        let note_ref = noted
            .marks
            .iter()
            .find(|m| m.name == MARK_NOTE_REF)
            .expect("footnote marker survives");
        assert_eq!(
            (note_ref.start, note_ref.end),
            (4, 5),
            "the marker covers the placeholder: {:?}",
            noted.marks
        );
        let note_id = match &note_ref.value {
            MarkValue::Str(s) => s.clone(),
            other => panic!("note-ref value is the note id, got {other:?}"),
        };
        assert_eq!(snap.notes.len(), 2, "both notes survive: {:?}", snap.notes);
        let footnote = snap
            .notes
            .iter()
            .find(|n| n.id == note_id)
            .expect("the marker names the footnote");
        assert_eq!(footnote.kind, "footnote");
        assert_eq!(footnote.blocks[0].text, "a footnote");
        let endnote = snap
            .notes
            .iter()
            .find(|n| n.kind == "endnote")
            .expect("endnote present");
        assert_eq!(endnote.blocks[0].text, "an endnote");

        assert_eq!(snap.comments.len(), 1, "{:?}", snap.comments);
        let comment = &snap.comments[0];
        assert_eq!(comment.author, "Ada Lovelace");
        assert_eq!(comment.blocks[0].text, "note: fix");
        assert_eq!(
            comment.range,
            Some((block(&snap, "please fix this word now").id.clone(), 11, 20))
        );
        assert_marks(
            &snap,
            "please fix this word now",
            &[(11, 20, MARK_COMMENT, MarkValue::Str(comment.id.clone()))],
        );

        assert_eq!(
            snap.changes.len(),
            2,
            "both changes survive: {:?}",
            snap.changes
        );
        let tracked_block = block(&snap, "hello there world");
        let ins_id = match &tracked_block
            .marks
            .iter()
            .find(|m| m.name == "ins")
            .expect("ins mark survives")
            .value
        {
            MarkValue::Str(s) => s.clone(),
            other => panic!("ins value {other:?}"),
        };
        let del_id = match &tracked_block
            .marks
            .iter()
            .find(|m| m.name == "del")
            .expect("del mark survives")
            .value
        {
            MarkValue::Str(s) => s.clone(),
            other => panic!("del value {other:?}"),
        };
        let insertion = snap
            .changes
            .iter()
            .find(|c| c.id == ins_id)
            .expect("ins change present");
        assert_eq!(insertion.kind, tilo_doc::snapshot::ChangeKind::Ins);
        assert_eq!(insertion.author, "Ada");
        assert_eq!(insertion.range, Some((tracked_block.id.clone(), 5, 11)));
        let deletion = snap
            .changes
            .iter()
            .find(|c| c.id == del_id)
            .expect("del change present");
        assert_eq!(deletion.kind, tilo_doc::snapshot::ChangeKind::Del);
        assert_eq!(deletion.author, "Bob");
        assert_eq!(deletion.range, Some((tracked_block.id.clone(), 12, 17)));

        let header = reopened.header();
        assert_eq!(header.len(), 1, "header block present: {header:?}");
        assert_eq!(header[0].text, "Title");
        let footer = reopened.footer();
        assert_eq!(footer.len(), 1, "footer block present: {footer:?}");
        assert_eq!(footer[0].text, format!("Page {FIELD_PLACEHOLDER}"));
        let field = footer[0]
            .marks
            .iter()
            .find(|m| m.name == tilo_doc::schema::MARK_FIELD)
            .expect("page-number field survives");
        assert_eq!(
            (field.start, field.end),
            (5, 6),
            "field spans the placeholder: {:?}",
            footer[0].marks
        );
        assert_eq!(field.value, MarkValue::Str(FIELD_PAGE_NUMBER.into()));
        let (columns, gap) = reopened.columns();
        assert_eq!(columns, 2, "column count survives the save");
        assert!(
            (gap.expect("gap present") - 48.0).abs() < 0.5,
            "column gap ≈ 48 px after the save, got {gap:?}"
        );
        assert_eq!(snap.columns, 2, "snapshot surfaces the column count");

        let props = reopened.doc_props();
        for (key, value) in [
            ("title", "My Title"),
            ("author", "Ada Lovelace"),
            ("subject", "Analytical Engine"),
            ("keywords", "computing, history"),
            ("created", "2020-01-02T03:04:05"),
            ("modified", "2021-06-07T08:09:10"),
            ("generator", "qdoc/1.0"),
        ] {
            assert_eq!(props.get(key).map(String::as_str), Some(value), "{key}");
        }

        // Saving what was reopened writes the same model again.
        let again = OdtCodec.export(reopened.as_ref(), &reimported).unwrap();
        let mut third = Document::new();
        let mut third_inputs = tilo_content::ContentInputs::new();
        OdtCodec
            .import(&again, &mut third.as_mut(), &mut third_inputs)
            .unwrap();
        assert_eq!(
            norm(&third.materialize()),
            norm(&snap),
            "the model is stable across a second save"
        );
        assert_eq!(
            reopened.doc_props(),
            third.doc_props(),
            "properties stable across a second save"
        );
        assert_eq!(
            norm_region(&third.header()),
            norm_region(&reopened.header()),
            "header stable across a second save"
        );
        assert_eq!(
            norm_region(&third.footer()),
            norm_region(&reopened.footer()),
            "footer stable across a second save"
        );
    }

    /// `.odt` files other editors wrote — including markup the model has no shape for — read into
    /// the model, and their unmodeled markup and page regions come back out unchanged.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one save and reopen of a single document is one flow"
    )]
    fn an_odt_from_another_editor_reads_into_the_model() {
        // A heading, a styled run and a list.
        let styles = r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>"#;
        let body = concat!(
            r#"<text:h text:outline-level="1">Title</text:h>"#,
            r#"<text:p>a <text:span text:style-name="T1">bold</text:span> b</text:p>"#,
            r#"<text:list><text:list-item><text:p>one</text:p></text:list-item>"#,
            r#"<text:list-item><text:p>two</text:p></text:list-item></text:list>"#,
        );
        let mut d = Document::new();
        import_odt(&minimal_odt_styled(styles, body), &mut d);
        let snap = d.materialize();
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert!(
            snap.blocks[1]
                .marks
                .iter()
                .any(|m| m.name == "strong" && m.start == 2 && m.end == 6),
            "{:?}",
            snap.blocks[1].marks
        );
        let exported = export_odt(&d);
        let mut d2 = Document::new();
        import_odt(&exported, &mut d2);
        assert_eq!(norm(&d2.materialize()), norm(&snap));

        // A footnote written inline.
        let note_body = concat!(
            r#"<text:p>see <text:note text:id="ftn1" text:note-class="footnote">"#,
            r#"<text:note-citation>1</text:note-citation>"#,
            r#"<text:note-body><text:p>the footnote text</text:p></text:note-body>"#,
            r#"</text:note>ref</text:p>"#,
        );
        let mut d = Document::new();
        import_odt(&minimal_odt(note_body), &mut d);
        let snap = d.materialize();
        assert_eq!(
            snap.blocks[0].text,
            format!("see {FIELD_PLACEHOLDER}ref"),
            "placeholder spliced inline: {:?}",
            snap.blocks[0].text
        );
        let nref = snap.blocks[0]
            .marks
            .iter()
            .find(|m| m.name == MARK_NOTE_REF)
            .expect("note-ref mark present");
        assert_eq!((nref.start, nref.end), (4, 5), "{:?}", snap.blocks[0].marks);
        let nid = match &nref.value {
            MarkValue::Str(s) => s.clone(),
            other => panic!("note-ref value is the note id, got {other:?}"),
        };
        assert_eq!(snap.notes.len(), 1, "{:?}", snap.notes);
        assert_eq!(snap.notes[0].id, nid, "marker references the note by id");
        assert_eq!(snap.notes[0].kind, "footnote");
        assert_eq!(snap.notes[0].blocks[0].text, "the footnote text");

        // A comment over a run, with its author and date.
        let comment_body = concat!(
            r#"<text:p>fix "#,
            r#"<office:annotation office:name="c1"><dc:creator>Ada</dc:creator><dc:date>2021-01-02T03:04:05</dc:date><text:p>note: fix</text:p></office:annotation>"#,
            r#"this word"#,
            r#"<office:annotation-end office:name="c1"/>"#,
            r#"</text:p>"#,
        );
        let mut d = Document::new();
        import_odt(&minimal_odt(comment_body), &mut d);
        let snap = d.materialize();
        assert_eq!(
            snap.blocks[0].text, "fix this word",
            "annotation markup not in text: {:?}",
            snap.blocks[0].text
        );
        let comment = snap.blocks[0]
            .marks
            .iter()
            .find(|m| m.name == MARK_COMMENT)
            .expect("comment mark present");
        assert_eq!(
            (comment.start, comment.end),
            (4, 13),
            "{:?}",
            snap.blocks[0].marks
        );
        assert_eq!(snap.comments.len(), 1, "{:?}", snap.comments);
        assert_eq!(snap.comments[0].author, "Ada");
        assert_eq!(snap.comments[0].date, "2021-01-02T03:04:05");
        assert_eq!(snap.comments[0].blocks[0].text, "note: fix");

        // Text a writer deleted, which the changed region carries: it sits in the body as a
        // deletion, and goes back to the changed region on export.
        let deleted_body = concat!(
            r#"<text:tracked-changes><text:changed-region xml:id="ct2" text:id="ct2"><text:deletion>"#,
            r#"<office:change-info><dc:creator>Bob</dc:creator></office:change-info><text:p>removed words</text:p>"#,
            r#"</text:deletion></text:changed-region></text:tracked-changes>"#,
            r#"<text:p>Tracked: <text:change text:change-id="ct2"/>gone.</text:p>"#,
        );
        let mut d = Document::new();
        import_odt(&minimal_odt(deleted_body), &mut d);
        let snap = d.materialize();
        assert_eq!(snap.blocks[0].text, "Tracked: removed wordsgone.");
        assert!(
            snap.blocks[0]
                .marks
                .iter()
                .any(|m| m.name == "del" && m.start == 9 && m.end == 22),
            "{:?}",
            snap.blocks[0].marks
        );
        let exported = export_odt(&d);
        let content = part(&exported, "content.xml");
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
        import_odt(&exported, &mut d2);
        assert_eq!(norm(&d2.materialize()), norm(&snap));

        // A table with merged cells.
        let table_body = concat!(
            r#"<table:table table:name="T"><table:table-column table:number-columns-repeated="3"/>"#,
            r#"<table:table-row><table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"><text:p>big</text:p></table:table-cell>"#,
            r#"<table:covered-table-cell/><table:table-cell><text:p>c</text:p></table:table-cell></table:table-row>"#,
            r#"<table:table-row><table:covered-table-cell table:number-columns-repeated="2"/><table:table-cell><text:p>f</text:p></table:table-cell></table:table-row>"#,
            r#"</table:table>"#,
        );
        let mut d = Document::new();
        import_odt(&minimal_odt(table_body), &mut d);
        let snap = d.materialize();
        let big = snap.blocks.iter().find(|b| b.text == "big").unwrap();
        assert_eq!(big.attrs.get("colspan"), Some(&AttrVal::Int(2)));
        assert_eq!(big.attrs.get("rowspan"), Some(&AttrVal::Int(2)));
        let f = snap.blocks.iter().find(|b| b.text == "f").unwrap();
        assert_eq!(f.attrs.get("col"), Some(&AttrVal::Int(2)), "{:?}", f.attrs);
        let exported = export_odt(&d);
        let content = part(&exported, "content.xml");
        assert!(
            content.contains(r#"<table:table-cell table:number-columns-spanned="2" table:number-rows-spanned="2"><text:p>big</text:p></table:table-cell><table:covered-table-cell/>"#),
            "{content}"
        );
        assert_eq!(content.matches("<table:covered-table-cell/>").count(), 3);
        let mut d2 = Document::new();
        import_odt(&exported, &mut d2);
        assert_eq!(norm(&d2.materialize()), norm(&snap));

        // Paragraph, span and table styles the model now agrees with come back out; one the
        // writer realigned is written from the model instead.
        let styles = concat!(
            r#"<style:style style:name="P1" style:family="paragraph"><style:paragraph-properties fo:break-before="page" fo:text-align="center"/></style:style>"#,
            r#"<style:style style:name="T1" style:family="text"><style:text-properties fo:letter-spacing="0.1cm" fo:font-weight="bold"/></style:style>"#,
            r#"<style:style style:name="Tbl" style:family="table"><style:table-properties style:width="10cm"/></style:style>"#,
            r#"<style:style style:name="Col" style:family="table-column"><style:table-column-properties style:column-width="5cm"/></style:style>"#,
            r#"<style:style style:name="Cell" style:family="table-cell"><style:table-cell-properties fo:border="0.5pt solid #000000"/></style:style>"#,
        );
        let table_with_styles = concat!(
            r#"<text:p text:style-name="P1">A <text:span text:style-name="T1">spaced</text:span> word</text:p>"#,
            r#"<text:p text:style-name="P1">second</text:p>"#,
            r#"<table:table table:name="T" table:style-name="Tbl"><table:table-column table:style-name="Col" table:number-columns-repeated="2"/>"#,
            r#"<table:table-row><table:table-cell table:style-name="Cell"><text:p>a</text:p></table:table-cell>"#,
            r#"<table:table-cell table:style-name="Cell"><text:p>b</text:p></table:table-cell></table:table-row></table:table>"#,
        );
        let mut d = Document::new();
        import_odt(&minimal_odt_styled(styles, table_with_styles), &mut d);
        let snap = d.materialize();
        assert!(
            snap.blocks[0]
                .marks
                .iter()
                .any(|m| m.name == "strong" && m.start == 2 && m.end == 8)
        );
        let second = snap.blocks[1].id.clone();
        d.as_mut().set_attr(&second, "align", "right");
        let exported = export_odt(&d);
        let content = part(&exported, "content.xml");
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
        import_odt(&exported, &mut d2);
        let again = d2.materialize();
        assert_eq!(norm(&again)[0], norm(&snap)[0]);
        assert_eq!(
            again.blocks[1].attrs.get("align"),
            Some(&AttrVal::Str("right".into()))
        );
    }

    /// The parts a `.odt` carries besides the body — settings, the styles an editor wrote, page
    /// regions, and metadata the model has no field for — survive a save, an edit, and a merge of
    /// two copies.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one save and reopen of a single document is one flow"
    )]
    fn preserved_parts_round_trip_and_converge() {
        // An unmodeled part is carried byte for byte.
        let base = minimal_odt_styled("", "<text:p>hi</text:p>");
        let mut package = ZipPackage::read(&base).unwrap();
        package.set(
            "settings.xml",
            b"<office:settings>custom</office:settings>".to_vec(),
        );
        let odt = package.write_odf().unwrap();
        let mut doc = Document::new();
        import_odt(&odt, &mut doc);
        let exported = export_odt(&doc);
        let out = ZipPackage::read(&exported).unwrap();
        assert_eq!(
            out.get("settings.xml"),
            Some(&b"<office:settings>custom</office:settings>"[..]),
            "unmodeled part must round-trip byte-identically"
        );
        assert_eq!(out.parts[0].0, "mimetype", "mimetype still first");
        assert!(out.get("content.xml").is_some(), "content regenerated");

        // Metadata the model has no field for is kept while the fields it has are updated, and a
        // document with no properties has no metadata part.
        let meta = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/"><office:meta><dc:title>Old</dc:title><meta:editing-cycles>7</meta:editing-cycles></office:meta></office:document-meta>"#;
        let mut package = ZipPackage::read(&minimal_odt_styled("", "<text:p>hi</text:p>")).unwrap();
        package.set("meta.xml", meta.as_bytes().to_vec());
        let odt = package.write_odf().unwrap();
        let mut doc = Document::new();
        import_odt(&odt, &mut doc);
        assert_eq!(
            doc.doc_prop("title").as_deref(),
            Some("Old"),
            "title imported"
        );
        doc.set_doc_prop("title", "New");
        doc.set_doc_prop("keywords", "k");
        let meta_out = part(&export_odt(&doc), "meta.xml");
        assert!(meta_out.contains("<dc:title>New</dc:title>"), "{meta_out}");
        assert!(!meta_out.contains("<dc:title>Old</dc:title>"), "{meta_out}");
        assert!(meta_out.contains("editing-cycles>7</"), "{meta_out}");
        assert!(
            meta_out.contains("<meta:keyword>k</meta:keyword>"),
            "{meta_out}"
        );
        let mut plain = Document::new();
        let paragraph = plain.insert_block(None, "paragraph");
        plain.splice_text(&paragraph, 0, 0, "plain");
        assert!(
            ZipPackage::read(&export_odt(&plain))
                .unwrap()
                .get("meta.xml")
                .is_none(),
            "no metadata part synthesised for a doc with no properties"
        );

        // A styles part with two style atoms each edited on its own replica converges.
        let odt = odt_with_two_style_atoms();
        let original_styles = ZipPackage::read(&odt)
            .unwrap()
            .get("styles.xml")
            .unwrap()
            .to_vec();
        let mut doc = Document::new();
        import_odt(&odt, &mut doc);
        assert_eq!(
            ZipPackage::read(&export_odt(&doc))
                .unwrap()
                .get("styles.xml")
                .unwrap(),
            original_styles.as_slice(),
            "segmented styles.xml re-emits byte-for-byte"
        );
        let root = doc.as_ref();
        assert!(
            tilo_doc::model::preserved_atoms(root.doc, root.c, "odt/part/styles.xml").len() >= 2,
            "styles.xml stored as multiple per-atom segments"
        );

        let mut a = Document::new();
        import_odt(&odt, &mut a);
        let mut b = a.fork();
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
        for (replica, who) in [(&a, "A"), (&b, "B")] {
            let styles = part(&export_odt(replica), "styles.xml");
            assert!(
                styles.contains("QuoteFont-EDITED"),
                "replica {who} kept A's Quote edit: {styles}"
            );
            assert!(
                styles.contains("CaptionFont-EDITED"),
                "replica {who} kept B's Caption edit: {styles}"
            );
            assert!(
                styles.find("QuoteFont-EDITED").unwrap()
                    < styles.find("CaptionFont-EDITED").unwrap(),
                "replica {who} kept atom order: {styles}"
            );
            assert!(
                styles.contains("<office:styles>") && styles.contains("</office:document-styles>")
            );
        }
        assert_eq!(
            part(&export_odt(&a), "styles.xml"),
            part(&export_odt(&b), "styles.xml"),
            "replicas converge on identical styles.xml"
        );

        // Page regions the writer did not edit keep their own elements; an edited one keeps the
        // rest of the master page.
        let styles = concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">"#,
            r#"<office:master-styles><style:master-page style:name="Standard">"#,
            r#"<style:header><text:p>Head</text:p></style:header>"#,
            r#"<style:header-first><text:p>Cover</text:p></style:header-first>"#,
            r#"<style:footer><text:p>Page <text:page-number>1</text:page-number> of <text:page-count>2</text:page-count></text:p></style:footer>"#,
            r#"</style:master-page></office:master-styles></office:document-styles>"#,
        );
        let mut package = ZipPackage::read(&minimal_odt("<text:p>body</text:p>")).unwrap();
        package.set("styles.xml", styles.as_bytes().to_vec());
        let odt = package.write_odf().unwrap();
        let mut doc = Document::new();
        import_odt(&odt, &mut doc);
        assert_eq!(
            part(&export_odt(&doc), "styles.xml"),
            styles,
            "unedited page regions keep their original elements"
        );
        let head = doc.header_blocks_in_order()[0].clone();
        doc.splice_header_text(&head, 4, 0, "er");
        let out = part(&export_odt(&doc), "styles.xml");
        assert!(out.contains("<text:p>Header</text:p>"), "{out}");
        assert!(
            out.contains("<text:page-count>2</text:page-count>"),
            "{out}"
        );

        // A master page that names a page layout keeps that name when its header is edited.
        let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="pm1"><style:header><text:p>Old Header</text:p></style:header></style:master-page></office:master-styles></office:document-styles>"#;
        let mut package = ZipPackage::read(&minimal_odt_styled("", "<text:p>hi</text:p>")).unwrap();
        package.set("styles.xml", styles_xml.as_bytes().to_vec());
        let odt = package.write_odf().unwrap();
        let mut doc = Document::new();
        import_odt(&odt, &mut doc);
        assert_eq!(
            doc.header()[0].text,
            "Old Header",
            "existing header imported"
        );
        let hid = doc.header_blocks_in_order()[0].clone();
        doc.splice_header_text(&hid, 0, 9, "New Header");
        let styles_out = part(&export_odt(&doc), "styles.xml");
        assert!(
            styles_out.contains(r#"style:page-layout-name="pm1""#),
            "page-layout reference preserved: {styles_out}"
        );
        assert!(
            styles_out.contains("New Header") && !styles_out.contains("Old Header"),
            "header updated: {styles_out}"
        );

        // Columns the model sets go into a page layout that keeps the writer's page size and
        // margins, and replace the ones already there rather than doubling them.
        let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"><office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties fo:page-width="21cm" fo:margin-left="2cm"><style:columns style:column-count="4"/></style:page-layout-properties></style:page-layout></office:automatic-styles></office:document-styles>"#;
        let mut package = ZipPackage::read(&minimal_odt_styled("", "<text:p>hi</text:p>")).unwrap();
        package.set("styles.xml", styles_xml.as_bytes().to_vec());
        let odt = package.write_odf().unwrap();
        let mut doc = Document::new();
        import_odt(&odt, &mut doc);
        assert_eq!(doc.columns().0, 4, "imported 4 columns");
        doc.set_columns(3, None);
        let styles_out = part(&export_odt(&doc), "styles.xml");
        assert!(
            styles_out.contains(r#"fo:page-width="21cm""#),
            "page size preserved: {styles_out}"
        );
        assert!(
            styles_out.contains(r#"fo:margin-left="2cm""#),
            "margin preserved: {styles_out}"
        );
        assert_eq!(
            styles_out.matches("<style:columns").count(),
            1,
            "exactly one style:columns: {styles_out}"
        );
        assert!(
            styles_out.contains(r#"style:column-count="3""#),
            "columns set from the model: {styles_out}"
        );
        let mut d2 = Document::new();
        import_odt(&export_odt(&doc), &mut d2);
        assert_eq!(d2.columns(), (3, None), "column count reads back");
    }
}
