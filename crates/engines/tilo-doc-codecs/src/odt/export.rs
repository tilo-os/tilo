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
