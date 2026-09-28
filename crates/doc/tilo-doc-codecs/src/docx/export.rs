//! Export the canonical model back to a `.docx`: emit `word/document.xml` via the inverse
//! mapping, re-emit every preserved part verbatim, and synthesise the content-types/rels/
//! numbering parts a from-scratch document lacks.

use super::container::DocxContainer;
use crate::format::CodecError;
use crate::preserve::{MARK_DOCX_XML, merge_root_namespaces};
use crate::resolved::Resolved;
use crate::xml_escape::{escape_attr, push_text};
use std::collections::{BTreeMap, BTreeSet};
use tilo_content::ContentSource;
use tilo_doc::export::InlinePlan;
use tilo_doc::model::DocRef;
use tilo_doc::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME,
    STYLE_SIZE, STYLE_SPACE_AFTER, STYLE_SPACE_BEFORE,
};
use tilo_doc::snapshot::{AttrVal, BlockView, DocSnapshot};
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

const CONTENT_TYPES_BASE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const NUMBERING_OVERRIDE: &str = r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#;
const NUMBERING_REL: &str = r#"<Relationship Id="rIdQdocNum" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#;

const CORE_OVERRIDE: &str = r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#;
const APP_OVERRIDE: &str = r#"<Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#;
const CORE_REL: &str = r#"<Relationship Id="rIdQdocCore" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>"#;
const APP_REL: &str = r#"<Relationship Id="rIdQdocApp" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/>"#;

const FOOTNOTES_OVERRIDE: &str = r#"<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>"#;
const ENDNOTES_OVERRIDE: &str = r#"<Override PartName="/word/endnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml"/>"#;
const FOOTNOTES_REL: &str = r#"<Relationship Id="rIdQdocFtnotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>"#;
const ENDNOTES_REL: &str = r#"<Relationship Id="rIdQdocEndnotes" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes" Target="endnotes.xml"/>"#;

const COMMENTS_OVERRIDE: &str = r#"<Override PartName="/word/comments.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml"/>"#;
const COMMENTS_REL: &str = r#"<Relationship Id="rIdQdocComments" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="comments.xml"/>"#;

const STYLES_OVERRIDE: &str = r#"<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>"#;
const STYLES_REL: &str = r#"<Relationship Id="rIdQdocStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#;
const STYLES_XML_OPEN: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#;

/// Which fixed-name parts a preserved `.docx` already declared, so export only injects a
/// content-type override / relationship for parts it's newly adding.
#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent 'was this fixed-name part already present' flag, \
              set by name in copy_preserved_parts, never positionally"
)]
struct PartFlags {
    had_content_types: bool,
    had_root_rels: bool,
    had_doc_rels: bool,
    had_numbering: bool,
    had_styles_part: bool,
    had_core: bool,
    had_app: bool,
    had_footnotes_part: bool,
    had_endnotes_part: bool,
    had_comments_part: bool,
}

/// Copies every preserved part into `c` verbatim, recording which fixed-name parts were present
/// and capturing the original `docProps/core.xml`/`app.xml` bytes for [`merge_styles_xml`]-style merging.
fn copy_preserved_parts(
    doc: &DocRef,
    content: &dyn ContentSource,
    c: &mut DocxContainer,
) -> (PartFlags, Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut flags = PartFlags::default();
    let mut original_core: Option<Vec<u8>> = None;
    let mut original_app: Option<Vec<u8>> = None;
    for (key, bytes) in tilo_doc::model::preserved_parts(doc.doc, doc.c, content) {
        if let Some(name) = key.strip_prefix("docx/part/") {
            flags.had_content_types |= name == "[Content_Types].xml";
            flags.had_root_rels |= name == "_rels/.rels";
            flags.had_doc_rels |= name == "word/_rels/document.xml.rels";
            flags.had_numbering |= name == "word/numbering.xml";
            flags.had_styles_part |= name == "word/styles.xml";
            flags.had_footnotes_part |= name == super::notes::FOOTNOTES_PART;
            flags.had_endnotes_part |= name == super::notes::ENDNOTES_PART;
            flags.had_comments_part |= name == super::comments::COMMENTS_PART;
            if name == "docProps/core.xml" {
                flags.had_core = true;
                original_core = Some(bytes.clone());
            }
            if name == "docProps/app.xml" {
                flags.had_app = true;
                original_app = Some(bytes.clone());
            }
            c.set(name, bytes);
        }
    }
    (flags, original_core, original_app)
}

/// The header and footer parts `section` writes qdoc's regions to.
fn region_parts(section: &super::section::SectionPlan) -> Vec<RegionPart> {
    let header = section.header_part.clone().map(|part| RegionPart {
        part,
        rel_id: super::headers::HEADER_REL_ID,
        rel_type: super::section::HEADER_REL_TYPE,
        content_type: super::section::HEADER_CONTENT_TYPE,
    });
    let footer = section.footer_part.clone().map(|part| RegionPart {
        part,
        rel_id: super::headers::FOOTER_REL_ID,
        rel_type: super::section::FOOTER_REL_TYPE,
        content_type: super::section::FOOTER_CONTENT_TYPE,
    });
    header.into_iter().chain(footer).collect()
}

/// A header or footer part this export writes, with the relationship that references it.
struct RegionPart {
    part: String,
    rel_id: &'static str,
    rel_type: &'static str,
    content_type: &'static str,
}

impl RegionPart {
    fn content_type_override(&self) -> String {
        super::section::override_xml(&self.part, self.content_type)
    }

    fn relationship(&self) -> String {
        super::section::relationship_xml(self.rel_id, self.rel_type, &self.part)
    }
}

/// Which optional parts this export is newly wiring up (synthesised but not already declared).
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent 'is this optional part newly wired up' flag, \
              set by name where it's decided, never positionally"
)]
struct Wireups {
    has_lists: bool,
    has_styles: bool,
    has_images: bool,
    wrote_core: bool,
    wrote_app: bool,
    need_styles: bool,
    need_core: bool,
    need_app: bool,
    /// qdoc's header/footer parts. Any header/footer relationship, part and override carried
    /// from the imported package that the new `w:sectPr` doesn't reference is already gone,
    /// so these are always wired up.
    regions: Vec<RegionPart>,
    need_footnotes: bool,
    need_endnotes: bool,
    need_comments: bool,
    /// External link relationships: (id, URL).
    links: Vec<(String, String)>,
}

/// Prefix of the relationship ids external links are written under.
const LINK_REL_PREFIX: &str = "rIdQdocLink";

/// Each distinct external link URL in the body → the relationship id it is written under.
fn link_relationship_ids(snap: &DocSnapshot) -> BTreeMap<String, String> {
    let mut urls: BTreeSet<&str> = BTreeSet::new();
    for b in &snap.blocks {
        for m in &b.marks {
            if m.name == tilo_doc::schema::MARK_LINK
                && let Some(href) = m.value.as_str()
                && !href.starts_with(tilo_doc::schema::CROSS_REF_PREFIX)
            {
                urls.insert(href);
            }
        }
    }
    urls.into_iter()
        .enumerate()
        .map(|(i, url)| {
            (
                url.to_string(),
                format!("{LINK_REL_PREFIX}{}", i.saturating_add(1)),
            )
        })
        .collect()
}

/// The `Relationship` of an external link.
fn link_relationship_xml(id: &str, url: &str) -> String {
    format!(
        r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="{}" TargetMode="External"/>"#,
        escape_attr(url)
    )
}

/// Builds/extends `[Content_Types].xml` with an `Override`/`Default` for every newly-wired part.
fn build_content_types(
    c: &mut DocxContainer,
    flags: &PartFlags,
    w: &Wireups,
    images: &ImageExport,
) {
    if flags.had_content_types {
        let mut overrides: Vec<(String, String)> = w
            .regions
            .iter()
            .map(|r| (format!("/{}", r.part), r.content_type_override()))
            .collect();
        let mut add = |part: &str, ovr: &str| overrides.push((part.to_string(), ovr.to_string()));
        if w.has_lists && !flags.had_numbering {
            add("/word/numbering.xml", NUMBERING_OVERRIDE);
        }
        if w.need_styles {
            add("/word/styles.xml", STYLES_OVERRIDE);
        }
        if w.need_core {
            add("/docProps/core.xml", CORE_OVERRIDE);
        }
        if w.need_app {
            add("/docProps/app.xml", APP_OVERRIDE);
        }
        if w.need_footnotes {
            add("/word/footnotes.xml", FOOTNOTES_OVERRIDE);
        }
        if w.need_endnotes {
            add("/word/endnotes.xml", ENDNOTES_OVERRIDE);
        }
        if w.need_comments {
            add("/word/comments.xml", COMMENTS_OVERRIDE);
        }
        let image_defaults = images.content_type_defaults();
        if (!overrides.is_empty() || !image_defaults.is_empty())
            && let Some(ct) = c.get("[Content_Types].xml")
        {
            let mut s = String::from_utf8_lossy(ct).into_owned();
            for (part, ovr) in overrides {
                if !s.contains(&format!(r#"PartName="{part}""#)) {
                    inject_before(&mut s, "</Types>", &ovr);
                }
            }
            // Image content-type `Default`s — dedup on `Extension="<ext>"`.
            for img in &images.images {
                let probe = format!(r#"Extension="{}""#, img.ext);
                if !s.contains(&probe) {
                    let (_, media) = image_ext_and_media(&img.bytes);
                    let def = format!(
                        r#"<Default Extension="{}" ContentType="{}"/>"#,
                        img.ext, media
                    );
                    inject_before(&mut s, "</Types>", &def);
                }
            }
            c.set("[Content_Types].xml", s.into_bytes());
        }
    } else {
        let mut ct = CONTENT_TYPES_BASE.to_string();
        if w.has_lists {
            ct.push_str(NUMBERING_OVERRIDE);
        }
        if w.has_styles {
            ct.push_str(STYLES_OVERRIDE);
        }
        if w.wrote_core {
            ct.push_str(CORE_OVERRIDE);
        }
        if w.wrote_app {
            ct.push_str(APP_OVERRIDE);
        }
        for r in &w.regions {
            ct.push_str(&r.content_type_override());
        }
        if w.need_footnotes {
            ct.push_str(FOOTNOTES_OVERRIDE);
        }
        if w.need_endnotes {
            ct.push_str(ENDNOTES_OVERRIDE);
        }
        if w.need_comments {
            ct.push_str(COMMENTS_OVERRIDE);
        }
        for def in images.content_type_defaults() {
            ct.push_str(&def);
        }
        ct.push_str("</Types>");
        c.set("[Content_Types].xml", ct.into_bytes());
    }
}

/// docProps relationships live in the root rels, not the document's; generated fresh or injected into an existing part.
fn build_root_rels(c: &mut DocxContainer, flags: &PartFlags, w: &Wireups) {
    if !flags.had_root_rels {
        let mut s = ROOT_RELS.to_string();
        if w.wrote_core {
            inject_before(&mut s, "</Relationships>", CORE_REL);
        }
        if w.wrote_app {
            inject_before(&mut s, "</Relationships>", APP_REL);
        }
        c.set("_rels/.rels", s.into_bytes());
    } else if (w.need_core || w.need_app)
        && let Some(rels) = c.get("_rels/.rels")
    {
        let mut s = String::from_utf8_lossy(rels).into_owned();
        if w.need_core && !s.contains("Target=\"docProps/core.xml\"") {
            inject_before(&mut s, "</Relationships>", CORE_REL);
        }
        if w.need_app && !s.contains("Target=\"docProps/app.xml\"") {
            inject_before(&mut s, "</Relationships>", APP_REL);
        }
        c.set("_rels/.rels", s.into_bytes());
    }
}

/// Needed once there's a numbering, freshly-synthesised styles, a header/footer, and/or a notes part.
fn build_document_rels(
    c: &mut DocxContainer,
    flags: &PartFlags,
    w: &Wireups,
    images: &ImageExport,
) {
    if !(w.has_lists
        || w.need_styles
        || !w.regions.is_empty()
        || w.need_footnotes
        || w.need_endnotes
        || w.need_comments
        || w.has_images
        || !w.links.is_empty())
    {
        return;
    }
    if !flags.had_doc_rels {
        let mut inner = String::new();
        if w.need_styles {
            inner.push_str(STYLES_REL);
        }
        if w.has_lists {
            inner.push_str(NUMBERING_REL);
        }
        for r in &w.regions {
            inner.push_str(&r.relationship());
        }
        if w.need_footnotes {
            inner.push_str(FOOTNOTES_REL);
        }
        if w.need_endnotes {
            inner.push_str(ENDNOTES_REL);
        }
        if w.need_comments {
            inner.push_str(COMMENTS_REL);
        }
        inner.push_str(&images.relationships());
        for (id, url) in &w.links {
            inner.push_str(&link_relationship_xml(id, url));
        }
        let rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{inner}</Relationships>"#
        );
        c.set("word/_rels/document.xml.rels", rels.into_bytes());
    } else if let Some(rels) = c.get("word/_rels/document.xml.rels") {
        let mut s = String::from_utf8_lossy(rels).into_owned();
        // Link relationships an earlier export wrote are replaced by this export's.
        if s.contains(LINK_REL_PREFIX) {
            s = super::section::remove_elements(&s, b"Relationship", |e| {
                e.attributes().flatten().any(|a| {
                    a.key.as_ref() == b"Id" && a.value.starts_with(LINK_REL_PREFIX.as_bytes())
                })
            });
        }
        for (id, url) in &w.links {
            inject_before(&mut s, "</Relationships>", &link_relationship_xml(id, url));
        }
        if w.has_lists && !s.contains("Target=\"numbering.xml\"") {
            inject_before(&mut s, "</Relationships>", NUMBERING_REL);
        }
        if w.need_styles && !s.contains("Target=\"styles.xml\"") {
            inject_before(&mut s, "</Relationships>", STYLES_REL);
        }
        for r in &w.regions {
            inject_before(&mut s, "</Relationships>", &r.relationship());
        }
        if w.need_footnotes && !s.contains("Target=\"footnotes.xml\"") {
            inject_before(&mut s, "</Relationships>", FOOTNOTES_REL);
        }
        if w.need_endnotes && !s.contains("Target=\"endnotes.xml\"") {
            inject_before(&mut s, "</Relationships>", ENDNOTES_REL);
        }
        if w.need_comments && !s.contains("Target=\"comments.xml\"") {
            inject_before(&mut s, "</Relationships>", COMMENTS_REL);
        }
        // Image rels — unique ids (`rIdImgN`), so a simple id-presence check dedups.
        for img in &images.images {
            if !s.contains(&format!(r#"Id="{}""#, img.rel_id)) {
                inject_before(
                    &mut s,
                    "</Relationships>",
                    &format!(
                        r#"<Relationship Id="{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{}"/>"#,
                        img.rel_id, img.target
                    ),
                );
            }
        }
        c.set("word/_rels/document.xml.rels", s.into_bytes());
    }
}

/// Copies each preserved-name-fixed part (header/footer/footnotes/endnotes/comments) that this
/// export freshly synthesised into `c`; the content-types/rels overrides for them are wired up
/// separately, once every "did we just add this part" flag is known.
fn copy_fixed_parts(
    c: &mut DocxContainer,
    hf_parts: super::headers::ExportedParts,
    section: &super::section::SectionPlan,
    notes: &super::notes::ExportedNotes,
    comments: &super::comments::ExportedComments,
) {
    if let (Some(hdr), Some(part)) = (hf_parts.header, section.header_part.as_deref()) {
        c.set(part, hdr);
    }
    if let (Some(ftr), Some(part)) = (hf_parts.footer, section.footer_part.as_deref()) {
        c.set(part, ftr);
    }
    if let Some(ftn) = notes.footnotes_xml.clone() {
        c.set(super::notes::FOOTNOTES_PART, ftn);
    }
    if let Some(end) = notes.endnotes_xml.clone() {
        c.set(super::notes::ENDNOTES_PART, end);
    }
    if let Some(cmt) = comments.comments_xml.clone() {
        c.set(super::comments::COMMENTS_PART, cmt);
    }
}

/// Regenerates `docProps/core.xml`/`app.xml` from canonical properties (merging any preserved
/// original's unmodeled children) and writes them into `c`. Returns whether each was written.
fn write_doc_props(
    c: &mut DocxContainer,
    doc: &DocRef,
    original_core: Option<&[u8]>,
    original_app: Option<&[u8]>,
) -> (bool, bool) {
    let wrote_core = if let Some(core_xml) = super::meta::export_core(doc, original_core) {
        c.set("docProps/core.xml", core_xml);
        true
    } else {
        false
    };
    let wrote_app = if let Some(app_xml) = super::meta::export_app(doc, original_app) {
        c.set("docProps/app.xml", app_xml);
        true
    } else {
        false
    };
    (wrote_core, wrote_app)
}

/// Writes `word/numbering.xml` (if the doc has any list) and merges referenced CRDT styles into
/// `word/styles.xml` (if any named style is referenced or a doc-default language is set).
fn write_lists_and_styles(
    c: &mut DocxContainer,
    has_lists: bool,
    has_styles: bool,
    numbering: &Numbering,
    named_refs: &BTreeSet<String>,
    snap: &Resolved<'_>,
    doc_lang: Option<&str>,
) {
    if let Some(xml) = numbering.numbering_xml(has_lists) {
        c.set("word/numbering.xml", xml.into_bytes());
    }
    // Merge referenced CRDT styles into the preserved styles.xml as `w:style` defs; already-declared styles are referenced, not redefined.
    if has_styles {
        let merged = merge_styles_xml(c.get("word/styles.xml"), named_refs, &snap.styles, doc_lang);
        c.set("word/styles.xml", merged);
    }
}

// `DocRef` is a two-pointer read-only view (like `&[T]`); `FormatProvider::export` hands one to
// every codec by value, and `DocxCodec::export` (mod.rs) forwards its own owned copy here
// unchanged, so re-borrowing it as `&DocRef` only to satisfy this lint adds an indirection with
// no cost saved.
#[allow(
    clippy::needless_pass_by_value,
    reason = "DocRef is two Copy pointers handed down by value from FormatProvider::export; re-borrowing buys nothing"
)]
pub(crate) fn export(doc: DocRef, content: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
    let snapshot = doc.materialize();
    let snap = &Resolved::new(&snapshot, content);
    let cols = super::columns::section_columns_xml(&doc).unwrap_or_default();

    let mut c = DocxContainer { parts: Vec::new() };
    let (flags, original_core, original_app) = copy_preserved_parts(&doc, content, &mut c);

    // The imported section properties arrive as a body fidelity block; they are rebuilt around
    // qdoc's header/footer and columns rather than copied, and header/footer parts carried
    // from the imported package that the rebuilt sectPr no longer references are pruned.
    let original_sectpr = snap
        .blocks
        .iter()
        .rev()
        .filter(|b| b.kind == "fidelity")
        .filter_map(|b| crate::preserve::fidelity_xml(b, "docx"))
        .find(|xml| super::section::is_section_properties(xml));
    let section_breaks: Vec<String> = snap
        .blocks
        .iter()
        .filter_map(|b| match b.attrs.get(super::paragraph::ATTR_PPR) {
            Some(AttrVal::Str(kept)) => super::paragraph::section_break(kept),
            _ => None,
        })
        .collect();
    let breaks: Vec<&str> = section_breaks.iter().map(String::as_str).collect();
    let keep = kept_regions(&doc, &c, &breaks, original_sectpr);
    let hf_parts = super::headers::export_parts(&doc, keep);
    let section = super::section::plan(&mut c, original_sectpr, &breaks, &hf_parts, keep, &cols);
    let numbering = Numbering::new(c.get("word/numbering.xml"), c.get("word/styles.xml"));
    let root = tilo_doc::model::preserved_part(
        doc.doc,
        doc.c,
        super::DOCUMENT_ROOT_KEY,
        &tilo_content::NoContent,
    )
    .and_then(|b| String::from_utf8(b).ok());
    let links = link_relationship_ids(snap);
    let notes = super::notes::export_notes(&doc);
    let comments = super::comments::export_comments(&doc);
    let (document, named_refs, numbering, images) = build_document_xml(
        snap,
        &section,
        numbering,
        &notes.id_map,
        &notes.kind_map,
        &comments.id_map,
        root.as_deref(),
    );
    let has_lists = snap.blocks.iter().any(|b| b.kind == "list_item");
    // A document default language forces styles.xml emission via `<w:docDefaults>`, so the language always has a home.
    let doc_lang = snap.language.clone();
    let has_styles = !named_refs.is_empty() || doc_lang.is_some();
    let has_images = !images.images.is_empty();

    c.set("word/document.xml", document.into_bytes());

    // Each exported drawing references a synthesised media part, named by its content.
    for img in &images.images {
        c.set(&img.part, img.bytes.clone());
    }

    let regions = region_parts(&section);
    copy_fixed_parts(&mut c, hf_parts, &section, &notes, &comments);

    let (wrote_core, wrote_app) = write_doc_props(
        &mut c,
        &doc,
        original_core.as_deref(),
        original_app.as_deref(),
    );

    write_lists_and_styles(
        &mut c,
        has_lists,
        has_styles,
        &numbering,
        &named_refs,
        snap,
        doc_lang.as_deref(),
    );

    let w = Wireups {
        has_lists,
        has_styles,
        has_images,
        wrote_core,
        wrote_app,
        need_styles: has_styles && !flags.had_styles_part,
        need_core: wrote_core && !flags.had_core,
        need_app: wrote_app && !flags.had_app,
        regions,
        need_footnotes: notes.footnotes_xml.is_some() && !flags.had_footnotes_part,
        need_endnotes: notes.endnotes_xml.is_some() && !flags.had_endnotes_part,
        need_comments: comments.comments_xml.is_some() && !flags.had_comments_part,
        links: links.into_iter().map(|(url, id)| (id, url)).collect(),
    };

    build_content_types(&mut c, &flags, &w, &images);
    build_root_rels(&mut c, &flags, &w);
    build_document_rels(&mut c, &flags, &w, &images);

    c.write()
}

/// Which imported default header/footer parts are written back as they are: those whose region
/// the model still holds unedited (see `headers::region_unchanged`). The parts are found the
/// way import found them, from the section properties and the carried relationships.
fn kept_regions(
    doc: &DocRef,
    c: &DocxContainer,
    section_breaks: &[&str],
    final_sectpr: Option<&str>,
) -> super::headers::KeptRegions {
    use tilo_doc::model::Region;
    let sections: String = section_breaks.iter().copied().chain(final_sectpr).collect();
    let (header, footer) = super::headers::resolve_default_parts(
        sections.as_bytes(),
        c.get("word/_rels/document.xml.rels"),
    );
    let unchanged = |part: Option<String>, region: Region| {
        part.and_then(|p| c.get(&p))
            .is_some_and(|xml| super::headers::region_unchanged(xml, region, doc))
    };
    super::headers::KeptRegions {
        header: unchanged(header, Region::Header),
        footer: unchanged(footer, Region::Footer),
    }
}

/// The `<w:docDefaults>` block declaring a document default language, read by
/// Word/LibreOffice for spell-check/screen-reader locale. Re-imported by `parse_styles`.
fn doc_defaults_lang_xml(lang: &str) -> String {
    format!(
        r#"<w:docDefaults><w:rPrDefault><w:rPr><w:lang w:val="{}"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
        escape_attr(lang)
    )
}

/// Build or extend `word/styles.xml` with `w:style` defs for the referenced container
/// styles; a style already declared there is left untouched. `doc_lang` is emitted as a
/// `<w:docDefaults>` block ahead of the styles only if one isn't already present — the
/// schema requires docDefaults precede styles.
fn merge_styles_xml(
    existing: Option<&[u8]>,
    refs: &BTreeSet<String>,
    styles: &BTreeMap<String, BTreeMap<String, AttrVal>>,
    doc_lang: Option<&str>,
) -> Vec<u8> {
    if let Some(bytes) = existing {
        let present = style_ids_in(bytes);
        let mut defs = String::new();
        for id in refs {
            if present.contains(id) {
                continue;
            }
            if let Some(def) = style_def_xml(id, styles) {
                defs.push_str(&def);
            }
        }
        let has_doc_defaults = bytes
            .windows(b"<w:docDefaults".len())
            .any(|w| w == b"<w:docDefaults");
        let defaults_inject = match doc_lang {
            Some(lang) if !has_doc_defaults => doc_defaults_lang_xml(lang),
            _ => String::new(),
        };
        if defs.is_empty() && defaults_inject.is_empty() {
            return bytes.to_vec();
        }
        let mut s = String::from_utf8_lossy(bytes).into_owned();
        if !defs.is_empty() {
            inject_before(&mut s, "</w:styles>", &defs);
        }
        // docDefaults must precede every `<w:style>`, so inject after the `<w:styles>` open tag.
        if !defaults_inject.is_empty() {
            inject_after_styles_open(&mut s, &defaults_inject);
        }
        s.into_bytes()
    } else {
        let mut s = STYLES_XML_OPEN.to_string();
        if let Some(lang) = doc_lang {
            s.push_str(&doc_defaults_lang_xml(lang));
        }
        if !refs.contains(NORMAL_STYLE_ID) {
            s.push_str(NORMAL_STYLE_XML);
        }
        for id in refs {
            if let Some(def) = style_def_xml(id, styles) {
                s.push_str(&def);
            }
        }
        s.push_str("</w:styles>");
        s.into_bytes()
    }
}

const NORMAL_STYLE_ID: &str = "Normal";
/// The default paragraph style a new styles part names, so a paragraph without
/// a `w:pStyle` has one in every reader.
const NORMAL_STYLE_XML: &str = r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>"#;

/// The definition for a referenced style id: the document's own style, else Word's built-in
/// heading for a `HeadingN` a heading block references, so other readers see a heading.
fn style_def_xml(id: &str, styles: &BTreeMap<String, BTreeMap<String, AttrVal>>) -> Option<String> {
    if let Some(props) = styles.get(id) {
        return Some(para_style_def_xml(id, props));
    }
    let level = id
        .strip_prefix("Heading")
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=6).contains(n))?;
    let half_points = match level {
        1 => 32,
        2 => 26,
        3 => 24,
        _ => 22,
    };
    let outline = level.saturating_sub(1);
    Some(format!(
        r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="heading {level}"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before="240" w:after="80"/><w:outlineLvl w:val="{outline}"/></w:pPr><w:rPr><w:b/><w:sz w:val="{half_points}"/></w:rPr></w:style>"#
    ))
}

/// Insert `frag` right after the `<w:styles …>` open tag, so `<w:docDefaults>` lands ahead
/// of every `<w:style>` as the schema requires.
fn inject_after_styles_open(s: &mut String, frag: &str) {
    if let Some(open) = s.find("<w:styles")
        && let Some(tail) = s.get(open..)
        && let Some(rel_close) = tail.find('>')
        && let Some(pos) = open.checked_add(rel_close).and_then(|p| p.checked_add(1))
    {
        s.insert_str(pos, frag);
    }
}

/// The `w:styleId` values already declared, so merging doesn't redefine them.
fn style_ids_in(xml: &[u8]) -> BTreeSet<String> {
    let s = String::from_utf8_lossy(xml);
    let mut out = BTreeSet::new();
    let needle = "w:styleId=\"";
    let mut rest = s.as_ref();
    while let Some(p) = rest.find(needle) {
        let Some(after_needle) = p.checked_add(needle.len()).and_then(|i| rest.get(i..)) else {
            break;
        };
        rest = after_needle;
        if let Some(end) = rest.find('"') {
            if let Some(id) = rest.get(..end) {
                out.insert(id.to_string());
            }
            let Some(after_quote) = end.checked_add(1).and_then(|i| rest.get(i..)) else {
                break;
            };
            rest = after_quote;
        } else {
            break;
        }
    }
    out
}

/// An OOXML `w:style` paragraph definition for a CRDT styles-container style, the inverse
/// of `styles::parse_styles` + `styles::style_props`.
fn para_style_def_xml(id: &str, props: &BTreeMap<String, AttrVal>) -> String {
    let getf = |k: &str| match props.get(k) {
        Some(AttrVal::Float(f)) => Some(*f),
        Some(AttrVal::Int(n)) => {
            #[allow(
                clippy::cast_precision_loss,
                reason = "n is a small style measurement (px/pt); always far below f64's 2^53 exact-integer bound"
            )]
            let v = *n as f64;
            Some(v)
        }
        _ => None,
    };
    let gets = |k: &str| match props.get(k) {
        Some(AttrVal::Str(s)) => Some(s.as_str()),
        _ => None,
    };
    let getb = |k: &str| matches!(props.get(k), Some(AttrVal::Bool(true)));

    let mut pp = String::new();
    if let Some(a) = gets(STYLE_ALIGN) {
        let v = match a {
            "center" => "center",
            "right" => "right",
            "justify" => "both",
            _ => "",
        };
        if !v.is_empty() {
            let _ = write!(pp, r#"<w:jc w:val="{v}"/>"#);
        }
    }
    let before = getf(STYLE_SPACE_BEFORE).filter(|v| *v != 0.0);
    let after = getf(STYLE_SPACE_AFTER).filter(|v| *v != 0.0);
    if before.is_some() || after.is_some() {
        pp.push_str("<w:spacing");
        if let Some(v) = before {
            let _ = write!(pp, r#" w:before="{}""#, px_to_twips(v));
        }
        if let Some(v) = after {
            let _ = write!(pp, r#" w:after="{}""#, px_to_twips(v));
        }
        pp.push_str("/>");
    }
    if let Some(v) = getf(STYLE_INDENT).filter(|v| *v != 0.0) {
        let _ = write!(pp, r#"<w:ind w:left="{}"/>"#, px_to_twips(v));
    }

    let mut rp = String::new();
    if let Some(f) = gets(STYLE_FONT) {
        let n = font_name(f);
        let _ = write!(rp, r#"<w:rFonts w:ascii="{n}" w:hAnsi="{n}"/>"#);
    }
    if getb(STYLE_BOLD) {
        rp.push_str("<w:b/>");
    }
    if getb(STYLE_ITALIC) {
        rp.push_str("<w:i/>");
    }
    if let Some(c) = gets(STYLE_COLOR) {
        let _ = write!(rp, r#"<w:color w:val="{}"/>"#, color_hex(c));
    }
    if let Some(sz) = getf(STYLE_SIZE) {
        let half_points = (sz * 2.0).round().clamp(0.0, 1_000_000.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped to [0, 1_000_000] above; a font size in half-points never approaches i64 range"
        )]
        let hp = half_points as i64;
        let _ = write!(rp, r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#);
    }

    let name = gets(STYLE_NAME).unwrap_or(id);
    let ppr = if pp.is_empty() {
        String::new()
    } else {
        format!("<w:pPr>{pp}</w:pPr>")
    };
    let rpr = if rp.is_empty() {
        String::new()
    } else {
        format!("<w:rPr>{rp}</w:rPr>")
    };
    format!(
        r#"<w:style w:type="paragraph" w:styleId="{}"><w:name w:val="{}"/>{ppr}{rpr}</w:style>"#,
        escape_attr(id),
        escape_attr(name)
    )
}

/// Pixels (96 dpi) → OOXML twips: the inverse of `styles::twips_to_px`. 1 px = 15 twips.
fn px_to_twips(px: f64) -> i64 {
    let twips = (px * 15.0).round().clamp(-1_000_000_000.0, 1_000_000_000.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to ±1e9 above; a document measurement in twips never approaches i64 range"
    )]
    let twips = twips as i64;
    twips
}

/// `#rrggbb` → OOXML `RRGGBB` (upper-case, no hash, as Word writes it); re-imports via `styles::normalize_color`.
fn color_hex(c: &str) -> String {
    c.trim_start_matches('#').to_ascii_uppercase()
}

fn inject_before(s: &mut String, needle: &str, frag: &str) {
    if let Some(pos) = s.rfind(needle) {
        s.insert_str(pos, frag);
    }
}

fn font_name(generic: &str) -> &'static str {
    match generic {
        "serif" => "Times New Roman",
        "mono" => "Courier New",
        _ => "Arial",
    }
}

/// Build the `<w:rPr>` body for an active mark set (empty if none modeled).
fn run_props(marks: &[&tilo_doc::document::MarkSpan]) -> String {
    let (mut bold, mut italic, mut underline, mut strike) = (false, false, false, false);
    let mut sz: Option<i64> = None;
    let mut font: Option<&str> = None;
    let mut color: Option<&str> = None;
    let mut highlight: Option<&str> = None;
    // Per-run language override (BCP-47, e.g. `fr`/`de-DE`) → `w:lang w:val`.
    let mut lang: Option<&str> = None;
    // Vertical alignment: `superscript`/`subscript` (super wins if both present).
    let mut vert_align: Option<&'static str> = None;
    for mark in marks {
        match mark.name.as_str() {
            "strong" => bold = true,
            "em" => italic = true,
            "underline" => underline = true,
            "strike" => strike = true,
            "fontSize" => sz = mark.value.as_int(),
            "fontFamily" => font = mark.value.as_str(),
            "color" => color = mark.value.as_str(),
            "highlight" => highlight = mark.value.as_str(),
            "lang" => lang = mark.value.as_str(),
            "super" => vert_align = Some("superscript"),
            "sub" if vert_align.is_none() => vert_align = Some("subscript"),
            _ => {}
        }
    }
    let mut s = String::new();
    if let Some(f) = font {
        let n = font_name(f);
        let _ = write!(s, r#"<w:rFonts w:ascii="{n}" w:hAnsi="{n}"/>"#);
    }
    if bold {
        s.push_str("<w:b/>");
    }
    if italic {
        s.push_str("<w:i/>");
    }
    if strike {
        s.push_str("<w:strike/>");
    }
    if let Some(c) = color {
        let _ = write!(s, r#"<w:color w:val="{}"/>"#, color_hex(c));
    }
    // w:shd carries the arbitrary hex highlight fill, since it round-trips any colour unlike the named w:highlight palette.
    if let Some(c) = highlight {
        let _ = write!(
            s,
            r#"<w:shd w:val="clear" w:color="auto" w:fill="{}"/>"#,
            color_hex(c)
        );
    }
    if let Some(n) = sz {
        let hp = n.saturating_mul(2);
        let _ = write!(s, r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#);
    }
    // BCP-47 tag emitted verbatim; Word uses it for spell-check locale and screen-reader language.
    if let Some(l) = lang {
        let _ = write!(s, r#"<w:lang w:val="{}"/>"#, escape_attr(l));
    }
    if underline {
        s.push_str(r#"<w:u w:val="single"/>"#);
    }
    if let Some(va) = vert_align {
        let _ = write!(s, r#"<w:vertAlign w:val="{va}"/>"#);
    }
    s
}

/// Emit run content, turning `\t` → `<w:tab/>` and `\n` → `<w:br/>`.
fn run_content(text: &str) -> String {
    let mut out = String::new();
    let mut buf = String::new();
    let flush = |out: &mut String, buf: &mut String| {
        if !buf.is_empty() {
            out.push_str(r#"<w:t xml:space="preserve">"#);
            for c in buf.chars() {
                push_text(out, c);
            }
            out.push_str("</w:t>");
            buf.clear();
        }
    };
    for c in text.chars() {
        match c {
            '\t' => {
                flush(&mut out, &mut buf);
                out.push_str("<w:tab/>");
            }
            '\n' => {
                flush(&mut out, &mut buf);
                out.push_str("<w:br/>");
            }
            _ => buf.push(c),
        }
    }
    flush(&mut out, &mut buf);
    out
}

/// The footnote/endnote reference run, carrying the FootnoteReference/EndnoteReference character style so Word superscripts it.
fn note_reference_run(
    note_id: &str,
    note_ids: &BTreeMap<String, i64>,
    note_kinds: &BTreeMap<String, tilo_doc::model::NoteKind>,
) -> Option<String> {
    let docx_id = note_ids.get(note_id)?;
    let kind = note_kinds
        .get(note_id)
        .copied()
        .unwrap_or(tilo_doc::model::NoteKind::Footnote);
    let (style, elem) = match kind {
        tilo_doc::model::NoteKind::Footnote => ("FootnoteReference", "w:footnoteReference"),
        tilo_doc::model::NoteKind::Endnote => ("EndnoteReference", "w:endnoteReference"),
    };
    Some(format!(
        r#"<w:r><w:rPr><w:rStyle w:val="{style}"/></w:rPr><{elem} w:id="{docx_id}"/></w:r>"#
    ))
}

/// A change-id → `&ChangeView` lookup for wrapping runs in `<w:ins>`/`<w:del>` on export.
type ChangeIndex<'a> = BTreeMap<String, &'a tilo_doc::snapshot::ChangeView>;

fn change_index(snap: &DocSnapshot) -> ChangeIndex<'_> {
    snap.changes.iter().map(|c| (c.id.clone(), c)).collect()
}

/// The `<w:ins>`/`<w:del>` open tag for a tracked change (docx id + author/date), wrapping the affected `<w:r>` runs.
fn change_open_tag(change: &tilo_doc::snapshot::ChangeView, docx_id: i64) -> String {
    let elem = match change.kind {
        tilo_doc::snapshot::ChangeKind::Ins => "w:ins",
        tilo_doc::snapshot::ChangeKind::Del => "w:del",
    };
    let date = if change.date.is_empty() {
        String::new()
    } else {
        format!(r#" w:date="{}""#, escape_attr(&change.date))
    };
    format!(
        r#"<{elem} w:id="{docx_id}" w:author="{}"{date}>"#,
        escape_attr(&change.author)
    )
}

/// Emit a *deleted* run's content as `<w:delText>` (OOXML requires it, not `w:t`); `\t`/`\n` still map as usual.
fn del_run_content(text: &str) -> String {
    let mut out = String::new();
    let mut buf = String::new();
    let flush = |out: &mut String, buf: &mut String| {
        if !buf.is_empty() {
            out.push_str(r#"<w:delText xml:space="preserve">"#);
            for c in buf.chars() {
                push_text(out, c);
            }
            out.push_str("</w:delText>");
            buf.clear();
        }
    };
    for c in text.chars() {
        match c {
            '\t' => {
                flush(&mut out, &mut buf);
                out.push_str("<w:tab/>");
            }
            '\n' => {
                flush(&mut out, &mut buf);
                out.push_str("<w:br/>");
            }
            _ => buf.push(c),
        }
    }
    flush(&mut out, &mut buf);
    out
}

/// The id-lookup tables `render_runs` needs to resolve marks into DOCX-side numeric/ref ids;
/// bundled so the function itself stays under the argument-count bar.
struct RunIds<'a, 'b> {
    note_ids: &'a BTreeMap<String, i64>,
    note_kinds: &'a BTreeMap<String, tilo_doc::model::NoteKind>,
    comment_ids: &'a BTreeMap<String, i64>,
    changes: &'a ChangeIndex<'b>,
    change_ids: &'a BTreeMap<String, i64>,
    bookmark_ids: &'a BTreeMap<String, i64>,
    /// External link URL → the relationship id it is written under.
    links: &'a BTreeMap<String, String>,
    /// The section plan, which section breaks kept in paragraph properties are rebuilt with.
    section: &'a super::section::SectionPlan,
}

/// Render a block's text + marks as `<w:r>` runs, starting a new run whenever the active
/// mark set changes. A `note-ref` placeholder becomes its reference-element run; a
/// `comment` mark emits range-start/end + reference; an `ins`/`del` mark wraps the run in
/// `<w:ins>`/`<w:del>` (deleted runs use `<w:delText>`).
fn render_runs(text: &str, marks: &[tilo_doc::document::MarkSpan], ids: &RunIds) -> String {
    let plan = InlinePlan::new(text, marks);
    let mut out = String::new();
    // Comment boundaries: ends (+ reference run) close before starts open, so
    // adjacent ranges don't collide.
    let emit_comment_boundaries =
        |out: &mut String, boundary: &tilo_doc::export::RangeBoundary<'_>| {
            for id in &boundary.ends {
                if let Some(end) = super::comments::comment_range_end(id, ids.comment_ids) {
                    out.push_str(&end);
                }
                if let Some(rf) = super::comments::comment_reference_run(id, ids.comment_ids) {
                    out.push_str(&rf);
                }
            }
            for id in &boundary.starts {
                if let Some(start) = super::comments::comment_range_start(id, ids.comment_ids) {
                    out.push_str(&start);
                }
            }
        };
    // Bookmark boundaries: end (by id) before start (id + name) — empty
    // elements sitting between runs.
    let emit_bookmark_boundaries =
        |out: &mut String, boundary: &tilo_doc::export::RangeBoundary<'_>| {
            for name in &boundary.ends {
                if let Some(id) = ids.bookmark_ids.get(*name) {
                    let _ = write!(out, r#"<w:bookmarkEnd w:id="{id}"/>"#);
                }
            }
            for name in &boundary.starts {
                if let Some(id) = ids.bookmark_ids.get(*name) {
                    let _ = write!(
                        out,
                        r#"<w:bookmarkStart w:id="{id}" w:name="{}"/>"#,
                        escape_attr(name)
                    );
                }
            }
        };

    for point in plan.points() {
        emit_comment_boundaries(&mut out, &point.boundary.comments);
        emit_bookmark_boundaries(&mut out, &point.boundary.bookmarks);
        let Some(run) = &point.run else {
            continue;
        };
        // A note-reference placeholder is its own run (no text), independent of the surrounding marks.
        if let Some(note_id) = run.note_reference {
            if let Some(reference) = note_reference_run(note_id, ids.note_ids, ids.note_kinds) {
                out.push_str(&reference);
            }
            continue;
        }
        // The `<w:ins>`/`<w:del>` wrapper (if any) for this run group; deleted text uses `<w:delText>`.
        let (wrap_open, wrap_close, is_del) = match run.change {
            Some(active_change) => match (
                ids.changes.get(active_change.id),
                ids.change_ids.get(active_change.id),
            ) {
                (Some(ch), Some(docx_id)) => {
                    let close = match active_change.kind {
                        tilo_doc::snapshot::ChangeKind::Ins => "</w:ins>",
                        tilo_doc::snapshot::ChangeKind::Del => "</w:del>",
                    };
                    (
                        change_open_tag(ch, *docx_id),
                        close,
                        active_change.kind == tilo_doc::snapshot::ChangeKind::Del,
                    )
                }
                _ => (String::new(), "", false),
            },
            None => (String::new(), "", false),
        };
        // A link wraps the whole run group in `<w:hyperlink>`, the outermost wrapper: a
        // cross-reference by `w:anchor`, an external link by its relationship's `r:id`.
        let external = run.link.and_then(|href| ids.links.get(href));
        let (xref_open, xref_close) = match (run.cross_reference(), external) {
            (Some(target), _) => (
                format!(r#"<w:hyperlink w:anchor="{}">"#, escape_attr(target)),
                "</w:hyperlink>",
            ),
            (None, Some(rid)) => (format!(r#"<w:hyperlink r:id="{rid}">"#), "</w:hyperlink>"),
            (None, None) => (String::new(), ""),
        };
        out.push_str(&xref_open);
        out.push_str(&wrap_open);
        push_run_group(&mut out, run, is_del);
        out.push_str(wrap_close);
        out.push_str(xref_close);
    }
    out
}

/// A block's `w:pPr`: the `modeled` children plus its alignment and line spacing, merged with
/// the properties kept verbatim from import.
fn block_ppr(b: &BlockView, modeled: &[(&[u8], String)], ids: &RunIds) -> String {
    let mut all = modeled.to_vec();
    all.push((b"w:jc", jc_xml(b)));
    all.push((b"w:spacing", spacing_xml(b)));
    let kept = match b.attrs.get(super::paragraph::ATTR_PPR) {
        Some(AttrVal::Str(kept)) => Some(kept.as_str()),
        _ => None,
    };
    super::paragraph::ppr_xml(&all, kept, |sectpr| ids.section.section_break(sectpr))
}

/// The runs of one run group: its text with the modeled and the kept run properties, after
/// the unmodeled inline element kept verbatim that the text shows, while the text still shows
/// it unedited (see `preserve::run_content`).
fn push_run_group(out: &mut String, run: &tilo_doc::export::InlineRun<'_>, is_del: bool) {
    let mark = |name: &str| {
        run.marks
            .iter()
            .find(|m| m.name == name)
            .and_then(|m| m.value.as_str())
    };
    let rpr =
        super::paragraph::run_props_xml(&run_props(&run.marks), mark(super::paragraph::MARK_RPR));
    let content = crate::preserve::run_content(run.text, &run.marks, MARK_DOCX_XML);
    if let Some(xml) = content.kept {
        out.push_str(xml);
    }
    if !content.text.is_empty() {
        push_run(out, &content.text, &rpr, is_del);
    }
}

/// One `w:r` of `text` with run properties `rpr`; deleted text uses `w:delText`.
fn push_run(out: &mut String, text: &str, rpr: &str, is_del: bool) {
    out.push_str("<w:r>");
    if !rpr.is_empty() {
        let _ = write!(out, "<w:rPr>{rpr}</w:rPr>");
    }
    if is_del {
        out.push_str(&del_run_content(text));
    } else {
        out.push_str(&run_content(text));
    }
    out.push_str("</w:r>");
}

fn jc_xml(b: &BlockView) -> String {
    match b.attrs.get("align") {
        Some(AttrVal::Str(a)) => match a.as_str() {
            "center" => r#"<w:jc w:val="center"/>"#.into(),
            "right" => r#"<w:jc w:val="right"/>"#.into(),
            "justify" => r#"<w:jc w:val="both"/>"#.into(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The `w:spacing` line-rule for `line_spacing` (multiple → 240ths, e.g. 1.5 → 360); re-imports via `line_spacing_from_spacing`.
fn spacing_xml(b: &BlockView) -> String {
    match b.attrs.get("line_spacing") {
        Some(AttrVal::Float(ls)) if *ls > 0.0 => {
            let line = (ls * 240.0).round().clamp(0.0, 1_000_000.0);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "clamped to [0, 1_000_000] above; a 240ths-of-a-line spacing value never approaches i64 range"
            )]
            let line = line as i64;
            format!(r#"<w:spacing w:line="{line}" w:lineRule="auto"/>"#)
        }
        _ => String::new(),
    }
}

fn cell_table(b: &BlockView) -> Option<String> {
    match b.attrs.get("table") {
        Some(AttrVal::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn cell_rc(b: &BlockView) -> (usize, usize) {
    let g = |k: &str| match b.attrs.get(k) {
        Some(AttrVal::Int(n)) => usize::try_from(*n).unwrap_or(0),
        _ => 0,
    };
    (g("row"), g("col"))
}

/// Map the model's numbering format onto `w:numFmt w:val` (inverse of `styles::map_num_format`).
fn ooxml_num_format(fmt: &str) -> &'static str {
    match fmt {
        "lower-roman" => "lowerRoman",
        "upper-roman" => "upperRoman",
        "lower-alpha" => "lowerLetter",
        "upper-alpha" => "upperLetter",
        _ => "decimal",
    }
}

/// The list numbering an export writes. An imported `word/numbering.xml` is kept whole — its
/// definitions stay valid for the list items that still match them (see `num_id`) and for
/// anything else referencing them (numbered heading styles, kept content) — and definitions
/// for the other list items are added to it, numbered past its own.
#[derive(Default)]
struct Numbering {
    /// The imported `word/numbering.xml`.
    original: Option<String>,
    /// What each of the original's `w:numId`s numbers: `(ordered, format, start)`.
    known: BTreeMap<String, (bool, String, i64)>,
    /// The first id past every `w:abstractNumId`/`w:numId` of the original.
    base: i64,
    /// `(ordered, format, start)` → added numId.
    ids: Vec<((bool, String, i64), i64)>,
    /// The package's styles define `ListParagraph`, the paragraph style Word gives list
    /// items. Referencing it undefined makes `LibreOffice` drop the items' numbering.
    list_paragraph_style: bool,
}

/// What a list item numbers: `(ordered, format, start)`.
fn list_key(b: &BlockView) -> (bool, String, i64) {
    let ordered = matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)));
    let fmt = match b.attrs.get("num_format") {
        Some(AttrVal::Str(s)) => s.clone(),
        _ => "decimal".to_string(),
    };
    let start = match b.attrs.get("num_start") {
        Some(AttrVal::Int(n)) => *n,
        _ => 1,
    };
    (ordered, fmt, start)
}

/// The largest numeric value of the attribute `key` (`w:numId=`) in `xml`.
fn max_attr_value(xml: &str, key: &str) -> Option<i64> {
    let needle = format!("{key}=\"");
    xml.match_indices(&needle)
        .filter_map(|(at, _)| {
            xml.get(at.saturating_add(needle.len())..)
                .and_then(|v| v.split('"').next())
                .and_then(|v| v.parse::<i64>().ok())
        })
        .max()
}

impl Numbering {
    fn new(original: Option<&[u8]>, styles: Option<&[u8]>) -> Self {
        let list_paragraph_style = styles.is_some_and(|s| {
            s.windows(br#"w:styleId="ListParagraph""#.len())
                .any(|w| w == br#"w:styleId="ListParagraph""#)
        });
        let Some(xml) = original.and_then(|b| std::str::from_utf8(b).ok()) else {
            return Self {
                list_paragraph_style,
                ..Self::default()
            };
        };
        let mut reg = super::styles::StyleRegistry::default();
        super::styles::parse_numbering(xml.as_bytes(), &mut reg);
        let known = reg
            .num_ordered
            .iter()
            .map(|(id, ordered)| {
                let def = reg.num_def.get(id);
                let fmt = def.and_then(|d| d.num_format).unwrap_or("decimal");
                let start = def.and_then(|d| d.num_start).unwrap_or(1);
                (id.clone(), (*ordered, fmt.to_string(), start))
            })
            .collect();
        let base = max_attr_value(xml, "w:abstractNumId")
            .max(max_attr_value(xml, "w:numId"))
            .map_or(1, |m| m.saturating_add(1));
        Self {
            original: Some(xml.to_string()),
            known,
            base,
            ids: Vec::new(),
            list_paragraph_style,
        }
    }

    /// The paragraph style reference of a list item without one of its own.
    fn paragraph_style(&self) -> String {
        if self.list_paragraph_style {
            r#"<w:pStyle w:val="ListParagraph"/>"#.to_string()
        } else {
            String::new()
        }
    }

    /// The `w:numId` for a list item: the imported one while it still numbers the item the
    /// same way, else one added for what the item numbers. Without an imported numbering,
    /// bullets and plain decimal lists use ids 1/2.
    fn num_id(&mut self, b: &BlockView) -> i64 {
        let key = list_key(b);
        if let Some(AttrVal::Str(id)) = b.attrs.get(super::content::ATTR_NUM)
            && let Some(known) = self.known.get(id)
            && known.0 == key.0
            && (!key.0 || (known.1 == key.1 && known.2 == key.2))
            && let Ok(n) = id.parse::<i64>()
        {
            return n;
        }
        if self.original.is_none() {
            if !key.0 {
                return 1;
            }
            if key.1 == "decimal" && key.2 == 1 {
                return 2;
            }
        }
        if let Some((_, id)) = self.ids.iter().find(|(k, _)| *k == key) {
            return *id;
        }
        let first = if self.original.is_some() {
            self.base
        } else {
            3
        };
        let id = i64::try_from(self.ids.len())
            .unwrap_or(i64::MAX)
            .saturating_add(first);
        self.ids.push((key, id));
        id
    }

    /// An abstract numbering (every level) and the `w:num` naming it, for `id`.
    fn definition(id: i64, (ordered, fmt, start): &(bool, String, i64)) -> (String, String) {
        let mut levels = String::new();
        for level in 0..9_u8 {
            if *ordered {
                let nf = ooxml_num_format(fmt);
                let _ = write!(
                    levels,
                    r#"<w:lvl w:ilvl="{level}"><w:start w:val="{start}"/><w:numFmt w:val="{nf}"/><w:lvlText w:val="%{}."/></w:lvl>"#,
                    level.saturating_add(1)
                );
            } else {
                let _ = write!(
                    levels,
                    r#"<w:lvl w:ilvl="{level}"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl>"#
                );
            }
        }
        (
            format!(r#"<w:abstractNum w:abstractNumId="{id}">{levels}</w:abstractNum>"#),
            format!(r#"<w:num w:numId="{id}"><w:abstractNumId w:val="{id}"/></w:num>"#),
        )
    }

    /// The `word/numbering.xml` to write: `None` when the imported one serves every item
    /// unchanged (or there are no lists and nothing was imported).
    fn numbering_xml(&self, has_lists: bool) -> Option<String> {
        let mut abs = String::new();
        let mut nums = String::new();
        if self.original.is_none() {
            if !has_lists {
                return None;
            }
            for (id, key) in [
                (1, (false, "decimal".to_string(), 1)),
                (2, (true, "decimal".to_string(), 1)),
            ] {
                let (a, n) = Self::definition(id, &key);
                abs.push_str(&a);
                nums.push_str(&n);
            }
        }
        for (key, id) in &self.ids {
            let (a, n) = Self::definition(*id, key);
            abs.push_str(&a);
            nums.push_str(&n);
        }
        match &self.original {
            None => Some(format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{abs}{nums}</w:numbering>"#
            )),
            Some(_) if self.ids.is_empty() => None,
            Some(original) => {
                // Abstract numberings precede the `w:num`s in `CT_Numbering`.
                let mut out = original.clone();
                let nums_at = out
                    .find("<w:num ")
                    .or_else(|| out.find("<w:numIdMacAtCleanup"))
                    .or_else(|| out.rfind("</w:numbering>"))?;
                out.insert_str(nums_at, &abs);
                let end = out
                    .find("<w:numIdMacAtCleanup")
                    .or_else(|| out.rfind("</w:numbering>"))?;
                out.insert_str(end, &nums);
                Some(out)
            }
        }
    }
}

/// EMUs per CSS pixel (914400 EMU/inch ÷ 96 px/inch).
const EMU_PER_PX: f64 = 914_400.0 / 96.0;

/// A media part + relationship synthesised for an exported image block.
struct ExportedImage {
    /// Relationship id (e.g. `rIdImg1`) referenced by the drawing's `a:blip`.
    rel_id: String,
    /// Package part name, e.g. `word/media/qdoc-0123456789abcdef.png`.
    part: String,
    /// `Target` written into the document rels (relative to `word/`).
    target: String,
    /// File extension (`png`/`jpg`/…) for the content-type `Default`.
    ext: &'static str,
    bytes: Vec<u8>,
}

/// Accumulator threaded through the body build: assigns rel ids + part names so `export()` can write parts and wire rels.
#[derive(Default)]
struct ImageExport {
    images: Vec<ExportedImage>,
}

impl ImageExport {
    /// The distinct `Default Extension`/`ContentType` rows the exported media parts need.
    fn content_type_defaults(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for img in &self.images {
            if seen.insert(img.ext) {
                let (_, media) = image_ext_and_media(&img.bytes);
                out.push(format!(
                    r#"<Default Extension="{}" ContentType="{}"/>"#,
                    img.ext, media
                ));
            }
        }
        out
    }

    /// The `<Relationship>` rows wiring each media part into the document rels.
    fn relationships(&self) -> String {
        let mut s = String::new();
        for img in &self.images {
            let _ = write!(
                s,
                r#"<Relationship Id="{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{}"/>"#,
                img.rel_id, img.target
            );
        }
        s
    }

    /// Register an image blob, returning its rel id; each drawing gets its own rel. The part is
    /// named by the image's content address, so it can never overwrite a different image the
    /// package carries through (a text box's picture, say) under a name of the `imageN` kind.
    fn add(&mut self, bytes: &[u8]) -> String {
        let n = self.images.len().saturating_add(1);
        let (ext, _) = image_ext_and_media(bytes);
        let rel_id = format!("rIdImg{n}");
        let hash = tilo_content::hash_bytes(bytes);
        let short = hash.get(..16).unwrap_or(&hash);
        let target = format!("media/qdoc-{short}.{ext}");
        self.images.push(ExportedImage {
            rel_id: rel_id.clone(),
            part: format!("word/{target}"),
            target,
            ext,
            bytes: bytes.to_vec(),
        });
        rel_id
    }
}

/// Sniff an image's extension + media type from its magic bytes.
fn image_ext_and_media(bytes: &[u8]) -> (&'static str, &'static str) {
    tilo_doc::export::detect_image_media(bytes)
        .filter(|media| matches!(media.extension, "png" | "jpg" | "gif"))
        .map_or(("bin", "application/octet-stream"), |media| {
            (media.extension, media.mime)
        })
}

/// The model `anchor` value of an image block (default `inline`).
fn image_anchor(b: &BlockView) -> &str {
    match b.attrs.get("anchor") {
        Some(AttrVal::Str(s)) => s.as_str(),
        _ => "inline",
    }
}

/// The model `wrap` value of an image block (default `none`).
fn image_wrap(b: &BlockView) -> &str {
    match b.attrs.get("wrap") {
        Some(AttrVal::Str(s)) => s.as_str(),
        _ => "none",
    }
}

/// An image block's display size in EMU, defaulting to a 1-inch square when unset.
fn image_extent_emu(b: &BlockView) -> (i64, i64) {
    let px = |k: &str, dflt: i64| match b.attrs.get(k) {
        Some(AttrVal::Int(n)) if *n > 0 => *n,
        _ => dflt,
    };
    // A document-supplied pixel dimension is clamped before the float round-trip, so neither
    // cast can lose more than the clamp already discards; a real image never approaches this bound.
    #[allow(
        clippy::cast_precision_loss,
        reason = "px is clamped to ±1e12 below, an image dimension never approaches f64's exact-integer bound"
    )]
    let emu = |px_val: i64| -> i64 {
        let px_val = px_val.clamp(-1_000_000_000_000, 1_000_000_000_000);
        let emu = (px_val as f64 * EMU_PER_PX).round().clamp(-1e15, 1e15);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped to ±1e15 above, far inside i64 range"
        )]
        let emu = emu as i64;
        emu
    };
    (emu(px("width", 96)), emu(px("height", 96)))
}

/// Build the `w:drawing` XML, registering its blob with `imgs`; inline emits `wp:inline`, floating emits `wp:anchor` + `wp:wrap*`.
fn image_drawing_xml(b: &BlockView, snap: &Resolved<'_>, imgs: &mut ImageExport) -> Option<String> {
    let Some(AttrVal::Str(hash)) = b.attrs.get("blob") else {
        return None;
    };
    let bytes = snap.image(hash)?;
    let rel_id = imgs.add(bytes);
    let (cx, cy) = image_extent_emu(b);
    let docpr_id = imgs.images.len(); // unique per drawing
    let name = escape_attr(&b.id);
    // The graphic payload (blip + shape) is shared by inline and anchored drawings.
    let graphic = format!(
        r#"<wp:docPr id="{docpr_id}" name="{name}"/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="{docpr_id}" name="{name}"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rel_id}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic>"#
    );
    let anchor = image_anchor(b);
    if anchor == "inline" {
        Some(format!(
            r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>{graphic}</wp:inline></w:drawing></w:r>"#
        ))
    } else {
        // Floating: positioned relative to the column (paragraph anchor) or page.
        let rel_from = if anchor == "page" { "page" } else { "column" };
        let rel_from_v = if anchor == "page" {
            "page"
        } else {
            "paragraph"
        };
        let wrap = match image_wrap(b) {
            "left" => r#"<wp:wrapSquare wrapText="left"/>"#.to_string(),
            "right" => r#"<wp:wrapSquare wrapText="right"/>"#.to_string(),
            "both" => r#"<wp:wrapSquare wrapText="bothSides"/>"#.to_string(),
            _ => r"<wp:wrapNone/>".to_string(),
        };
        Some(format!(
            r#"<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="0" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="{rel_from}"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="{rel_from_v}"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>{wrap}{graphic}</wp:anchor></w:drawing></w:r>"#
        ))
    }
}

/// A block-sequence index, widened to the `i64` these XML ids use; never realistically exceeds
/// `i64::MAX` (a document would need over 9 quintillion blocks), so a fallback is unreachable in practice.
fn index_to_i64(i: usize) -> i64 {
    i64::try_from(i).unwrap_or(i64::MAX)
}

/// Renders one contiguous run of `cell` blocks sharing a `table` id as a `w:tbl`, returning the
/// index just past the run.
fn render_table_block(body: &mut String, snap: &Resolved<'_>, i: usize, ids: &RunIds) -> usize {
    let Some(b) = snap.blocks.get(i) else {
        return i.saturating_add(1);
    };
    let tid = cell_table(b).unwrap_or_default();
    let start = i;
    let mut i = i;
    while snap
        .blocks
        .get(i)
        .is_some_and(|blk| cell_table(blk).as_deref() == Some(tid.as_str()))
    {
        i = i.saturating_add(1);
    }
    let Some(cells) = snap.blocks.get(start..i) else {
        return i;
    };
    // Cells by grid position; the rows below a vertically merged cell hold a continuation
    // cell as wide as it.
    let mut at: BTreeMap<(usize, usize), &BlockView> = BTreeMap::new();
    let mut continued: BTreeMap<(usize, usize), &BlockView> = BTreeMap::new();
    let (mut nrows, mut ncols) = (0usize, 0usize);
    for cell in cells {
        let (r, c) = cell_rc(cell);
        let (cols, rows) = (cell_span(cell, "colspan"), cell_span(cell, "rowspan"));
        at.entry((r, c)).or_insert(cell);
        for dr in 1..rows {
            continued.insert((r.saturating_add(dr), c), cell);
        }
        nrows = nrows.max(r.saturating_add(rows));
        ncols = ncols.max(c.saturating_add(cols));
    }
    body.push_str("<w:tbl>");
    push_table_props(body, cells.first(), ncols);
    // A row's properties ride on its first cell.
    let mut row_props: BTreeMap<usize, &str> = BTreeMap::new();
    for cell in cells {
        if let Some(AttrVal::Str(xml)) = cell.attrs.get(super::paragraph::ATTR_TRPR) {
            row_props.entry(cell_rc(cell).0).or_insert(xml.as_str());
        }
    }
    for r in 0..nrows {
        body.push_str("<w:tr>");
        if let Some(pr) = row_props.get(&r) {
            body.push_str(pr);
        }
        let mut c = 0;
        while c < ncols {
            let own = at.get(&(r, c)).copied();
            // A vertical merge's continuation cells look like its first one.
            let cell = own.or_else(|| continued.get(&(r, c)).copied());
            let (span, merge, inner) = if let Some(cl) = own {
                let merge = if cell_span(cl, "rowspan") > 1 {
                    r#"<w:vMerge w:val="restart"/>"#
                } else {
                    ""
                };
                (
                    cell_span(cl, "colspan"),
                    merge,
                    render_runs(&cl.text, &cl.marks, ids),
                )
            } else if let Some(first) = cell {
                (cell_span(first, "colspan"), "<w:vMerge/>", String::new())
            } else {
                (1, "", String::new())
            };
            let grid_span = if span > 1 {
                format!(r#"<w:gridSpan w:val="{span}"/>"#)
            } else {
                String::new()
            };
            let kept = |key: &str| match cell.and_then(|cl| cl.attrs.get(key)) {
                Some(AttrVal::Str(xml)) => Some(xml.as_str()),
                _ => None,
            };
            let tcpr = super::paragraph::cell_props_xml(
                &[
                    (b"w:tcW", super::paragraph::DEFAULT_TCW.to_string()),
                    (b"w:gridSpan", grid_span),
                    (b"w:vMerge", merge.to_string()),
                ],
                kept(super::paragraph::ATTR_TCPR),
            );
            let ppr = super::paragraph::ppr_xml(
                &[],
                kept(super::paragraph::ATTR_PPR),
                ToString::to_string,
            );
            let _ = write!(body, "<w:tc>{tcpr}<w:p>{ppr}{inner}</w:p></w:tc>");
            c = c.saturating_add(span);
        }
        body.push_str("</w:tr>");
    }
    body.push_str("</w:tbl>");
    i
}

/// A table's `w:tblPr` and `w:tblGrid`: the ones kept from import on its `first` cell (the
/// grid only while it still has a column per grid column), else automatic ones.
fn push_table_props(body: &mut String, first: Option<&BlockView>, ncols: usize) {
    let kept = |key: &str| match first.and_then(|c| c.attrs.get(key)) {
        Some(AttrVal::Str(xml)) => Some(xml.as_str()),
        _ => None,
    };
    body.push_str(kept(super::paragraph::ATTR_TBLPR).unwrap_or(super::paragraph::DEFAULT_TBLPR));
    if let Some(grid) = kept(super::paragraph::ATTR_TBLGRID)
        .filter(|grid| super::paragraph::grid_columns(grid) == ncols)
    {
        body.push_str(grid);
        return;
    }
    body.push_str("<w:tblGrid>");
    for _ in 0..ncols {
        body.push_str("<w:gridCol/>");
    }
    body.push_str("</w:tblGrid>");
}

/// A cell's `colspan`/`rowspan` (1 when absent).
fn cell_span(b: &BlockView, key: &str) -> usize {
    match b.attrs.get(key) {
        Some(AttrVal::Int(n)) if *n > 1 => usize::try_from(*n).unwrap_or(1).min(1024),
        _ => 1,
    }
}

/// Renders one block (or, for `cell`, a whole contiguous table run) into `body`, returning the
/// index of the next unprocessed block.
fn render_block(
    body: &mut String,
    snap: &Resolved<'_>,
    i: usize,
    numbering: &mut Numbering,
    imgs: &mut ImageExport,
    named_refs: &mut BTreeSet<String>,
    ids: &RunIds,
) -> usize {
    let Some(b) = snap.blocks.get(i) else {
        return i.saturating_add(1);
    };
    match b.kind.as_str() {
        "heading" => {
            let level = match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => (*n).clamp(1, 6),
                _ => 1,
            };
            let style = format!(r#"<w:pStyle w:val="Heading{level}"/>"#);
            named_refs.insert(format!("Heading{level}"));
            let ppr = block_ppr(b, &[(b"w:pStyle", style)], ids);
            let _ = write!(
                body,
                "<w:p>{ppr}{}</w:p>",
                render_runs(&b.text, &b.marks, ids)
            );
            i.saturating_add(1)
        }
        "list_item" => {
            let numid = numbering.num_id(b);
            let level = match b.attrs.get("level") {
                Some(AttrVal::Int(n)) => (*n).clamp(0, 8),
                _ => 0,
            };
            // A list item keeps its own paragraph style when it had one.
            let style = match b.attrs.get(super::paragraph::ATTR_PPR) {
                Some(AttrVal::Str(kept)) if super::paragraph::has_style(kept) => String::new(),
                _ => numbering.paragraph_style(),
            };
            let num = format!(
                r#"<w:numPr><w:ilvl w:val="{level}"/><w:numId w:val="{numid}"/></w:numPr>"#
            );
            let ppr = block_ppr(b, &[(b"w:pStyle", style), (b"w:numPr", num)], ids);
            let _ = write!(
                body,
                "<w:p>{ppr}{}</w:p>",
                render_runs(&b.text, &b.marks, ids)
            );
            i.saturating_add(1)
        }
        "image" => {
            // Wrap the drawing run in a paragraph (Word images live in a `w:p`).
            if let Some(drawing) = image_drawing_xml(b, snap, imgs) {
                let _ = write!(body, "<w:p>{drawing}</w:p>");
            }
            i.saturating_add(1)
        }
        "fidelity" => {
            // Section properties are rebuilt and appended after the last block.
            if let Some(xml) = crate::preserve::fidelity_xml(b, "docx")
                && !super::section::is_section_properties(xml)
            {
                body.push_str(xml);
            }
            i.saturating_add(1)
        }
        "cell" => render_table_block(body, snap, i, ids),
        _ => {
            // A plain paragraph: a `style` attr emits `<w:pStyle>`; a direct `align` attr still rides as `<w:jc>`.
            let mut style = String::new();
            if let Some(AttrVal::Str(sid)) = b.attrs.get("style")
                && snap.styles.contains_key(sid)
            {
                style = format!(r#"<w:pStyle w:val="{}"/>"#, escape_attr(sid));
                named_refs.insert(sid.clone());
            }
            let ppr = block_ppr(b, &[(b"w:pStyle", style)], ids);
            let _ = write!(
                body,
                "<w:p>{ppr}{}</w:p>",
                render_runs(&b.text, &b.marks, ids)
            );
            i.saturating_add(1)
        }
    }
}

/// Build `word/document.xml`, collecting referenced style ids, the numbering plan, and each note id's docx id + kind.
fn build_document_xml(
    snap: &Resolved<'_>,
    section: &super::section::SectionPlan,
    mut numbering: Numbering,
    note_ids: &BTreeMap<String, i64>,
    note_kinds: &BTreeMap<String, tilo_doc::model::NoteKind>,
    comment_ids: &BTreeMap<String, i64>,
    root: Option<&str>,
) -> (String, BTreeSet<String>, Numbering, ImageExport) {
    let links = &link_relationship_ids(snap);
    let mut body = String::new();
    let mut named_refs: BTreeSet<String> = BTreeSet::new();
    let mut imgs = ImageExport::default();
    // Tracked-change ids are assigned from 0 in canonical (sorted) order, independent of comment ids.
    let changes = change_index(snap);
    let change_ids: BTreeMap<String, i64> = snap
        .changes
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.clone(), index_to_i64(i)))
        .collect();
    // Bookmark ids are assigned in sorted-name order so output is deterministic, from the
    // first id no verbatim-kept bookmark uses.
    let bookmark_ids: BTreeMap<String, i64> = {
        let base = first_free_bookmark_id(snap);
        let mut names: Vec<String> = snap.bookmarks.iter().map(|bm| bm.name.clone()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name, index_to_i64(i).saturating_add(base)))
            .collect()
    };
    let ids = RunIds {
        note_ids,
        note_kinds,
        comment_ids,
        changes: &changes,
        change_ids: &change_ids,
        bookmark_ids: &bookmark_ids,
        links,
        section,
    };
    let mut i = 0;
    while i < snap.blocks.len() {
        i = render_block(
            &mut body,
            snap,
            i,
            &mut numbering,
            &mut imgs,
            &mut named_refs,
            &ids,
        );
    }

    // The imported document's root keeps its namespace declarations (and `mc:Ignorable`), so
    // the elements carried through verbatim resolve; the ones this exporter writes are added.
    let root = root
        .and_then(|r| merge_root_namespaces(r, "w:document", DOCUMENT_NAMESPACES))
        .unwrap_or_else(|| {
            let mut tag = String::from("<w:document");
            for (prefix, uri) in DOCUMENT_NAMESPACES {
                let _ = write!(tag, r#" xmlns:{prefix}="{uri}""#);
            }
            tag.push('>');
            tag
        });
    // sectPr is a direct child of `w:body`, placed after the last block.
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
{root}<w:body>{body}{}</w:body></w:document>"#,
        section.sectpr
    );
    (document, named_refs, numbering, imgs)
}

/// The namespaces the exported `word/document.xml` declares on its root.
const DOCUMENT_NAMESPACES: &[(&str, &str)] = &[
    ("w", W_NS),
    (
        "r",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    ),
    (
        "m",
        "http://schemas.openxmlformats.org/officeDocument/2006/math",
    ),
    (
        "wp",
        "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
    ),
    (
        "mc",
        "http://schemas.openxmlformats.org/markup-compatibility/2006",
    ),
    ("o", "urn:schemas-microsoft-com:office:office"),
    ("v", "urn:schemas-microsoft-com:vml"),
    ("w10", "urn:schemas-microsoft-com:office:word"),
    (
        "wne",
        "http://schemas.microsoft.com/office/word/2006/wordml",
    ),
];

/// The largest `w:id` of a `w:bookmarkStart` inside XML carried through verbatim, so the
/// bookmarks the exporter numbers itself start above it and never collide.
fn max_bookmark_id(xml: &str) -> Option<i64> {
    let mut max = None;
    let mut rest = xml;
    while let Some(at) = rest.find("<w:bookmarkStart") {
        rest = rest.get(at.saturating_add(1)..).unwrap_or_default();
        let tag = rest
            .get(..rest.find('>').unwrap_or(rest.len()))
            .unwrap_or_default();
        let id = tag
            .find("w:id=\"")
            .and_then(|p| tag.get(p.saturating_add(6)..))
            .and_then(|v| v.split('"').next())
            .and_then(|v| v.parse::<i64>().ok());
        max = max.max(id);
    }
    max
}

/// The first bookmark id free of every id used by verbatim XML in `snap`.
fn first_free_bookmark_id(snap: &DocSnapshot) -> i64 {
    let verbatim = snap.blocks.iter().flat_map(|b| {
        let block = crate::preserve::fidelity_xml(b, "docx");
        let inline = b
            .marks
            .iter()
            .filter(|m| m.name == MARK_DOCX_XML)
            .filter_map(|m| m.value.as_str());
        block.into_iter().chain(inline)
    });
    verbatim
        .filter_map(max_bookmark_id)
        .max()
        .map_or(0, |m| m.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::super::DocxCodec;
    use super::super::container::{DocxContainer, minimal_docx, minimal_docx_styled};
    use crate::FormatProvider;
    use std::collections::BTreeMap;
    use tilo_doc::document::MarkValue;
    use tilo_doc::{AttrVal, DocSnapshot, Document};

    /// Import `bytes` into `doc`, dropping any binary inputs (these tests reference none).
    fn import_docx(bytes: &[u8], doc: &mut Document) {
        DocxCodec
            .import(
                bytes,
                &mut doc.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
    }

    /// Export `doc` with no binary content available.
    fn export_docx(doc: &Document) -> Vec<u8> {
        DocxCodec
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

    type NormRegion = Vec<(String, Vec<(usize, usize, String, MarkValue)>)>;

    /// Normalize a region's blocks, dropping the volatile per-import block id so imports compare equal.
    fn norm_region(blocks: &[tilo_doc::snapshot::BlockView]) -> NormRegion {
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
    fn text_and_paragraph_formatting_roundtrips_through_docx() {
        // Phase `export_import_roundtrips_model`.
        {
            let body = concat!(
                r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
                r#"<w:p><w:r><w:t xml:space="preserve">a </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:t xml:space="preserve"> b</w:t></w:r></w:p>"#,
            );
            let docx = minimal_docx_styled("", body);
            let mut d = Document::new();
            import_docx(&docx, &mut d);
            let m1 = norm(&d.materialize());

            let exported = export_docx(&d);
            let c = DocxContainer::read(&exported).unwrap();
            assert!(c.get("word/document.xml").is_some());
            assert!(c.get("[Content_Types].xml").is_some());
            assert!(c.get("_rels/.rels").is_some());

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            let m2 = norm(&d2.materialize());
            assert_eq!(m1, m2, "model round-trips through .docx export then import");
        }
        // Phase `a_heading_defines_the_built_in_style_it_references`.
        {
            let mut d = Document::new();
            let h = d.insert_block(None, "heading");
            d.splice_text(&h, 0, 0, "Q3 Planning");
            d.as_mut().set_attr(&h, "level", 2_i64);
            let c = DocxContainer::read(&export_docx(&d)).unwrap();
            let styles = String::from_utf8(c.get("word/styles.xml").unwrap().to_vec()).unwrap();
            assert!(styles.contains(r#"w:styleId="Heading2""#), "{styles}");
            assert!(
                styles.contains(r#"<w:name w:val="heading 2"/>"#),
                "{styles}"
            );
            assert!(styles.contains(r#"<w:outlineLvl w:val="1"/>"#), "{styles}");
            assert!(
                styles.contains(r#"w:default="1" w:styleId="Normal""#),
                "a new styles part names the default paragraph style: {styles}"
            );
            let content_types = c.get("[Content_Types].xml").unwrap();
            assert!(
                String::from_utf8_lossy(content_types).contains("/word/styles.xml"),
                "the styles part is declared"
            );
        }
        // Phase `heading_paragraph_props_roundtrip`.
        {
            let mut d = Document::new();
            let h = d.insert_block(None, "heading");
            d.splice_text(&h, 0, 0, "Head");
            let p = d.insert_block(Some(&h), "paragraph");
            d.splice_text(&p, 0, 0, "styled");
            {
                let mut dm = d.as_mut();
                dm.set_attr(&h, "level", 3_i64);
                dm.set_attr(&p, "align", "center");
                dm.add_mark(&p, 0, 6, "fontSize", 22_i64);
                dm.add_mark(&p, 0, 6, "fontFamily", "serif");
                dm.add_mark(&p, 0, 6, "underline", true);
            }

            let exported = export_docx(&d);
            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            let snap = d2.materialize();
            assert_eq!(snap.blocks[0].kind, "heading");
            assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(3)));
            let b = &snap.blocks[1];
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
            assert!(
                b.marks.iter().any(|m| m.name == "underline"),
                "{:?}",
                b.marks
            );
        }
        // Phase `highlight_super_and_line_spacing_roundtrip`.
        {
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "E=mc2");
            {
                let mut dm = d.as_mut();
                dm.add_mark(&id, 0, 5, "highlight", "#ffff00");
                dm.add_mark(&id, 4, 5, "super", true);
                dm.set_attr(&id, "line_spacing", 1.5_f64);
            }

            let exported = export_docx(&d);
            let c = DocxContainer::read(&exported).unwrap();
            let xml = std::str::from_utf8(c.get("word/document.xml").unwrap()).unwrap();
            assert!(
                xml.contains(r#"w:shd w:val="clear" w:color="auto" w:fill="FFFF00""#),
                "highlight shd: {xml}"
            );
            assert!(
                xml.contains(r#"<w:vertAlign w:val="superscript"/>"#),
                "super vertAlign: {xml}"
            );
            assert!(
                xml.contains(r#"<w:spacing w:line="360" w:lineRule="auto"/>"#),
                "line spacing: {xml}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
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
        // Phase `subscript_roundtrips_through_docx`.
        {
            let mut d = Document::new();
            let id = d.insert_block(None, "paragraph");
            d.splice_text(&id, 0, 0, "H2O");
            d.as_mut().add_mark(&id, 1, 2, "sub", true);

            let exported = export_docx(&d);
            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
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
        // Phase `colored_serif_20pt_justified_run_roundtrips_through_docx`.
        {
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
            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
            assert!(
                document.contains(r#"<w:jc w:val="both"/>"#),
                "justify → w:jc both: {document}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
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
        // Phase `named_paragraph_style_roundtrips_through_docx`.
        {
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

            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
            assert!(
                document.contains(r#"<w:pStyle w:val="Fancy"/>"#),
                "paragraph references the named style: {document}"
            );
            let styles = std::str::from_utf8(cont.get("word/styles.xml").unwrap()).unwrap();
            assert!(
                styles.contains(r#"<w:style w:type="paragraph" w:styleId="Fancy">"#),
                "named style defined as OOXML w:style: {styles}"
            );
            assert!(
                styles.contains("<w:b/>")
                    && styles.contains("<w:i/>")
                    && styles.contains(r#"<w:sz w:val="40"/>"#),
                "props emitted: {styles}"
            );
            assert!(
                styles.contains(r#"<w:jc w:val="center"/>"#)
                    && styles.contains(r#"<w:color w:val="FF0000"/>"#),
                "pPr/colour emitted: {styles}"
            );
            let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
            assert!(
                ct.contains("/word/styles.xml"),
                "styles override registered: {ct}"
            );
            let rels =
                std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
            assert!(
                rels.contains(r#"Target="styles.xml""#),
                "styles relationship registered: {rels}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
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

            let exported2 = export_docx(&d2);
            let mut d3 = Document::new();
            import_docx(&exported2, &mut d3);
            assert_eq!(
                norm(&d2.materialize()),
                norm(&d3.materialize()),
                "named-style model is stable across a second round-trip"
            );
        }
    }

    // 1×1 PNG blob for the image round-trip tests.
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
    fn images_roundtrip_through_docx() {
        // Phase `image_inline_roundtrips_through_docx`.
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
                dm.set_attr(&img, "width", 64_i64);
                dm.set_attr(&img, "height", 48_i64);
            }

            let exported = DocxCodec.export(d.as_ref(), &inputs).unwrap();
            let cont = DocxContainer::read(&exported).unwrap();
            let media = cont
                .parts
                .iter()
                .find(|(n, _)| {
                    n.starts_with("word/media/")
                        && std::path::Path::new(n)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
                })
                .expect("media part written");
            assert_eq!(media.1, &TINY_PNG[..], "image bytes written verbatim");
            let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
            assert!(
                document.contains("<wp:inline"),
                "inline drawing emitted: {document}"
            );
            let rels =
                std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
            assert!(
                rels.contains(&format!(
                    r#"Target="media/qdoc-{}.png""#,
                    &tilo_content::hash_bytes(&TINY_PNG)[..16]
                )),
                "image rel wired: {rels}"
            );
            let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
            assert!(
                ct.contains(r#"Extension="png""#),
                "png content-type default: {ct}"
            );

            let mut d2 = Document::new();
            let mut reimported = tilo_content::ContentInputs::new();
            DocxCodec
                .import(&exported, &mut d2.as_mut(), &mut reimported)
                .unwrap();
            let snap = d2.materialize();
            let im = snap
                .blocks
                .iter()
                .find(|b| b.kind == "image")
                .expect("image block present");
            let blob_hash = match im.attrs.get("blob") {
                Some(AttrVal::Str(h)) => h.clone(),
                other => panic!("image missing blob attr: {other:?}"),
            };
            assert_eq!(blob_hash, hash, "blake3 hash stable across round-trip");
            assert_eq!(
                reimported.get(&blob_hash),
                Some(&TINY_PNG[..]),
                "blob bytes survive"
            );
            assert_eq!(
                im.attrs.get("anchor"),
                Some(&AttrVal::Str("inline".into())),
                "{:?}",
                im.attrs
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
        }
        // Phase `image_positioning_roundtrips_through_docx`.
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

            let exported = DocxCodec.export(d.as_ref(), &inputs).unwrap();
            let cont = DocxContainer::read(&exported).unwrap();
            let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
            assert!(
                document.contains("<wp:anchor"),
                "anchor drawing emitted: {document}"
            );
            assert!(
                document.contains(r#"<wp:wrapSquare wrapText="right"/>"#),
                "right wrap emitted: {document}"
            );

            let mut d2 = Document::new();
            let mut reimported = tilo_content::ContentInputs::new();
            DocxCodec
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
    fn lists_tables_and_cross_references_roundtrip_through_docx() {
        // Phase `lists_roundtrip_with_ordering`.
        {
            let mut d = Document::new();
            let a = d.insert_block(None, "list_item");
            d.splice_text(&a, 0, 0, "first");
            let b = d.insert_block(Some(&a), "list_item");
            d.splice_text(&b, 0, 0, "second");
            {
                let mut dm = d.as_mut();
                dm.set_attr(&a, "ordered", true);
                dm.set_attr(&b, "ordered", false);
            }

            let exported = export_docx(&d);
            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            let snap = d2.materialize();
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|x| x.kind == "list_item")
                .collect();
            assert_eq!(items.len(), 2, "{snap:?}");
            assert_eq!(items[0].text, "first");
            assert_eq!(items[0].attrs.get("ordered"), Some(&AttrVal::Bool(true)));
            assert_eq!(items[1].attrs.get("ordered"), Some(&AttrVal::Bool(false)));
        }
        // Phase `list_numbering_format_and_start_roundtrip_through_docx`.
        {
            // Upper-roman ordered list starting at 3: format + start must round-trip.
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

            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            let numbering = std::str::from_utf8(cont.get("word/numbering.xml").unwrap()).unwrap();
            assert!(
                numbering.contains(r#"<w:numFmt w:val="upperRoman"/>"#),
                "upper-roman numFmt emitted: {numbering}"
            );
            assert!(
                numbering.contains(r#"<w:start w:val="3"/>"#),
                "start value emitted: {numbering}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            let snap = d2.materialize();
            let items: Vec<_> = snap
                .blocks
                .iter()
                .filter(|x| x.kind == "list_item")
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

            let exported2 = export_docx(&d2);
            let mut d3 = Document::new();
            import_docx(&exported2, &mut d3);
            assert_eq!(
                norm(&d2.materialize()),
                norm(&d3.materialize()),
                "list-numbering model stable across a second round-trip"
            );
        }
        // Phase `table_roundtrips_as_editable_cells`.
        {
            let mut d = Document::new();
            let mut prev: Option<String> = None;
            for (r, c, t) in [(0, 0, "a"), (0, 1, "b"), (1, 0, "c"), (1, 1, "d")] {
                let id = d.insert_block(prev.as_deref(), "cell");
                d.splice_text(&id, 0, 0, t);
                {
                    let mut dm = d.as_mut();
                    dm.set_attr(&id, "table", "T1");
                    dm.set_attr(&id, "row", i64::from(r));
                    dm.set_attr(&id, "col", i64::from(c));
                }
                prev = Some(id);
            }
            let exported = export_docx(&d);
            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            let snap = d2.materialize();
            let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
            assert_eq!(
                cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(),
                vec!["a", "b", "c", "d"],
                "{snap:?}"
            );
            assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
            assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));
            // Cells stay editable: their text lives in a real Text object.
            let id = &cells[0].id;
            d2.splice_text(id, 1, 0, "X");
            assert_eq!(d2.block_text(id).as_deref(), Some("aX"));
        }
        // Phase `bookmark_and_cross_reference_roundtrip_through_docx`.
        {
            let mut d = Document::new();
            let target = d.insert_block(None, "paragraph");
            d.splice_text(&target, 0, 0, "the intro section");
            d.add_bookmark(&target, 4, 9, "intro"); // over "intro"
            let src = d.insert_block(Some(&target), "paragraph");
            d.splice_text(&src, 0, 0, "see above");
            d.add_cross_ref(&src, 4, 9, "intro"); // "above" → #intro

            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
            assert!(
                document.contains(r#"<w:bookmarkStart w:id="0" w:name="intro"/>"#),
                "bookmarkStart emitted: {document}"
            );
            assert!(
                document.contains(r#"<w:bookmarkEnd w:id="0"/>"#),
                "bookmarkEnd emitted: {document}"
            );
            assert!(
                document.contains(r#"<w:hyperlink w:anchor="intro">"#),
                "cross-ref hyperlink emitted: {document}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn document_properties_and_unmodeled_parts_roundtrip_through_docx() {
        // Phase `document_properties_roundtrip_through_docx`.
        {
            // Title/author/subject/keywords/dates/generator export into core.xml + app.xml generator.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "body");
            d.set_doc_prop("title", "My Title");
            d.set_doc_prop("author", "Ada Lovelace");
            d.set_doc_prop("subject", "Analytical Engine");
            d.set_doc_prop("keywords", "computing, history");
            d.set_doc_prop("created", "2020-01-02T03:04:05Z");
            d.set_doc_prop("modified", "2021-06-07T08:09:10Z");
            d.set_doc_prop("generator", "qdoc/1.0");

            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            let core =
                std::str::from_utf8(cont.get("docProps/core.xml").expect("core.xml written"))
                    .unwrap();
            assert!(
                core.contains("<dc:title>My Title</dc:title>"),
                "title in core.xml: {core}"
            );
            assert!(
                core.contains("<dc:creator>Ada Lovelace</dc:creator>"),
                "creator: {core}"
            );
            assert!(
                core.contains("<dc:subject>Analytical Engine</dc:subject>"),
                "subject: {core}"
            );
            assert!(
                core.contains("<cp:keywords>computing, history</cp:keywords>"),
                "keywords: {core}"
            );
            assert!(
                core.contains("2020-01-02T03:04:05Z</dcterms:created>"),
                "created: {core}"
            );
            assert!(
                core.contains("2021-06-07T08:09:10Z</dcterms:modified>"),
                "modified: {core}"
            );
            let app = std::str::from_utf8(cont.get("docProps/app.xml").expect("app.xml written"))
                .unwrap();
            assert!(
                app.contains("<Application>qdoc/1.0</Application>"),
                "generator in app.xml: {app}"
            );
            let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
            assert!(
                ct.contains("/docProps/core.xml") && ct.contains("/docProps/app.xml"),
                "overrides registered: {ct}"
            );
            let rels = std::str::from_utf8(cont.get("_rels/.rels").unwrap()).unwrap();
            assert!(
                rels.contains(r#"Target="docProps/core.xml""#)
                    && rels.contains(r#"Target="docProps/app.xml""#),
                "rels registered: {rels}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
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
                Some("2020-01-02T03:04:05Z")
            );
            assert_eq!(
                props.get("modified").map(String::as_str),
                Some("2021-06-07T08:09:10Z")
            );
            assert_eq!(props.get("generator").map(String::as_str), Some("qdoc/1.0"));
            assert_eq!(d2.materialize().blocks[0].text, "body");

            let exported2 = export_docx(&d2);
            let mut d3 = Document::new();
            import_docx(&exported2, &mut d3);
            assert_eq!(
                d2.doc_props(),
                d3.doc_props(),
                "properties stable across a second round-trip"
            );
        }
        // Phase `core_xml_preserves_unmodeled_fields_and_updates_modeled`.
        {
            // An imported unmodeled field (revision) must survive while the title updates on export.
            let core = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/"><dc:title>Old</dc:title><cp:revision>4</cp:revision></cp:coreProperties>"#;
            let base = minimal_docx_styled("", "<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
            let mut c = DocxContainer::read(&base).unwrap();
            c.set("docProps/core.xml", core.as_bytes().to_vec());
            let docx = c.write().unwrap();

            let mut d = Document::new();
            import_docx(&docx, &mut d);
            assert_eq!(
                d.doc_prop("title").as_deref(),
                Some("Old"),
                "title imported from core.xml"
            );
            d.set_doc_prop("title", "New");

            let exported = export_docx(&d);
            let out = DocxContainer::read(&exported).unwrap();
            let core_out = std::str::from_utf8(out.get("docProps/core.xml").unwrap()).unwrap();
            assert!(
                core_out.contains("<dc:title>New</dc:title>"),
                "title updated: {core_out}"
            );
            assert!(
                !core_out.contains("<dc:title>Old</dc:title>"),
                "old title gone: {core_out}"
            );
            assert!(
                core_out.contains("revision>4</"),
                "unmodeled field preserved: {core_out}"
            );
        }
        // Phase `no_docprops_when_no_properties_and_none_imported`.
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "plain");
            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            assert!(
                cont.get("docProps/core.xml").is_none(),
                "no core.xml for a doc with no properties"
            );
            assert!(
                cont.get("docProps/app.xml").is_none(),
                "no app.xml for a doc with no properties"
            );
        }
        // Phase `preserves_unmodeled_parts`.
        {
            let base = minimal_docx_styled("", "<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
            let mut c = DocxContainer::read(&base).unwrap();
            c.set("customXml/item1.xml", b"<root>custom</root>".to_vec());
            let docx = c.write().unwrap();

            let mut d = Document::new();
            import_docx(&docx, &mut d);
            let exported = export_docx(&d);

            let out = DocxContainer::read(&exported).unwrap();
            assert_eq!(
                out.get("customXml/item1.xml"),
                Some(&b"<root>custom</root>"[..]),
                "unmodeled part must round-trip byte-identically"
            );
            assert!(out.get("word/document.xml").is_some(), "body regenerated");
            assert_eq!(out.get("word/styles.xml"), c.get("word/styles.xml"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn headers_and_footers_with_page_fields_roundtrip_through_docx() {
        // Phase `header_footer_with_page_number_roundtrip_through_docx`.
        {
            use tilo_doc::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_FIELD};
            // A header "Title" + page-number footer must export wired through sectPr and re-import intact.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "body");
            let h = d.insert_header_block(None, "paragraph");
            d.splice_header_text(&h, 0, 0, "Title");
            let f = d.insert_footer_block(None, "paragraph");
            d.splice_footer_text(&f, 0, 0, "Page ");
            d.insert_footer_page_number(&f, 5);

            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            let hdr =
                std::str::from_utf8(cont.get("word/header1.xml").expect("header1.xml written"))
                    .unwrap();
            assert!(
                hdr.contains("<w:hdr") && hdr.contains("Title"),
                "header part holds the title: {hdr}"
            );
            let ftr =
                std::str::from_utf8(cont.get("word/footer1.xml").expect("footer1.xml written"))
                    .unwrap();
            assert!(ftr.contains("<w:ftr"), "footer part is a w:ftr: {ftr}");
            assert!(
                ftr.contains(r#"w:instr=" PAGE ""#),
                "PAGE field emitted: {ftr}"
            );
            let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
            assert!(
                document.contains("<w:sectPr>"),
                "sectPr present: {document}"
            );
            assert!(
                document.contains(r#"<w:headerReference w:type="default" r:id="rIdQdocHdr"/>"#),
                "header ref: {document}"
            );
            assert!(
                document.contains(r#"<w:footerReference w:type="default" r:id="rIdQdocFtr"/>"#),
                "footer ref: {document}"
            );
            let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
            assert!(
                ct.contains("/word/header1.xml") && ct.contains("/word/footer1.xml"),
                "overrides registered: {ct}"
            );
            let rels =
                std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
            assert!(
                rels.contains(r#"Target="header1.xml""#)
                    && rels.contains(r#"Target="footer1.xml""#),
                "rels registered: {rels}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            assert_eq!(d2.materialize().blocks[0].text, "body", "body intact");
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

            // Idempotent across a second round-trip (block ids are volatile; compare text+marks, not BlockView).
            let exported2 = export_docx(&d2);
            let mut d3 = Document::new();
            import_docx(&exported2, &mut d3);
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
        // Phase `imports_complex_page_field_from_header`.
        {
            // Complex-field form (fldChar begin/instrText PAGE/separate/end) imports as a single
            // page-number field, dropping the cached result text.
            use tilo_doc::schema::{FIELD_PLACEHOLDER, MARK_FIELD};
            let header = concat!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                "\n",
                r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
                r#"<w:p><w:r><w:t xml:space="preserve">p. </w:t></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
                r#"<w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
                r#"<w:r><w:t>7</w:t></w:r>"#,
                r#"<w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:hdr>"#,
            );
            let body = concat!(
                r#"<w:p><w:r><w:t>body</w:t></w:r></w:p>"#,
                r#"<w:sectPr><w:headerReference w:type="default" r:id="rId7"/></w:sectPr>"#,
            );
            let base = minimal_docx_styled("", body);
            let mut c = DocxContainer::read(&base).unwrap();
            c.set("word/header1.xml", header.as_bytes().to_vec());
            let rels = std::str::from_utf8(c.get("word/_rels/document.xml.rels").unwrap())
                .unwrap()
                .to_string();
            let rels = rels.replace(
            "</Relationships>",
            r#"<Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#,
        );
            c.set("word/_rels/document.xml.rels", rels.into_bytes());
            let docx = c.write().unwrap();

            let mut d = Document::new();
            import_docx(&docx, &mut d);
            let header = d.header();
            assert_eq!(header.len(), 1, "{header:?}");
            // "p. " + a single placeholder; the cached "7" result is dropped.
            assert_eq!(
                header[0].text,
                format!("p. {FIELD_PLACEHOLDER}"),
                "complex field → one placeholder: {:?}",
                header[0].text
            );
            assert!(
                header[0]
                    .marks
                    .iter()
                    .any(|m| m.name == MARK_FIELD && m.start == 3 && m.end == 4),
                "field mark: {:?}",
                header[0].marks
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn notes_comments_and_tracked_changes_roundtrip_through_docx() {
        // Phase `footnote_imports_from_footnotes_part`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::{FIELD_PLACEHOLDER, MARK_NOTE_REF};
            // "see ref" referencing footnote id 2 in word/footnotes.xml: import must place a
            // note-ref marker + create the note.
            let body = concat!(
                r#"<w:p>"#,
                r#"<w:r><w:t xml:space="preserve">see </w:t></w:r>"#,
                r#"<w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="2"/></w:r>"#,
                r#"<w:r><w:t>ref</w:t></w:r>"#,
                r#"</w:p>"#,
            );
            let footnotes = concat!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                "\n",
                r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
                r#"<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>"#,
                r#"<w:footnote w:id="2"><w:p><w:r><w:t>the footnote</w:t></w:r></w:p></w:footnote>"#,
                r#"</w:footnotes>"#,
            );
            let base = minimal_docx(body);
            let mut c = DocxContainer::read(&base).unwrap();
            c.set("word/footnotes.xml", footnotes.as_bytes().to_vec());
            let docx = c.write().unwrap();

            let mut d = Document::new();
            import_docx(&docx, &mut d);
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
                "marker covers the placeholder: {:?}",
                p.marks
            );
            let nid = match &nref.value {
                MarkValue::Str(s) => s.clone(),
                other => panic!("note-ref value is the note id: {other:?}"),
            };
            assert_eq!(
                snap.notes.len(),
                1,
                "one note, separators skipped: {:?}",
                snap.notes
            );
            assert_eq!(snap.notes[0].id, nid);
            assert_eq!(snap.notes[0].kind, "footnote");
            assert_eq!(snap.notes[0].blocks[0].text, "the footnote");
        }
        // Phase `footnote_and_endnote_roundtrip_through_docx`.
        {
            // A footnote + an endnote built from scratch: export must write both note parts + refs
            // + content-types/rels, re-importing the same markers/notes/kinds.
            let exported = export_footnote_endnote_fixture();
            assert_footnote_endnote_docx_xml(&exported);

            let d2 = assert_footnote_endnote_reimport(&exported);

            let exported2 = export_docx(&d2);
            let mut d3 = Document::new();
            import_docx(&exported2, &mut d3);
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
        // Phase `comment_imports_from_comments_part`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::MARK_COMMENT;
            // "fix this word" with "this word" wrapped in comment range id 0: import must place a
            // `comment` mark + create the comment.
            let body = concat!(
                r#"<w:p>"#,
                r#"<w:r><w:t xml:space="preserve">fix </w:t></w:r>"#,
                r#"<w:commentRangeStart w:id="0"/>"#,
                r#"<w:r><w:t>this word</w:t></w:r>"#,
                r#"<w:commentRangeEnd w:id="0"/>"#,
                r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="0"/></w:r>"#,
                r#"</w:p>"#,
            );
            let comments = concat!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                "\n",
                r#"<w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
                r#"<w:comment w:id="0" w:author="Ada" w:date="2021-01-02T03:04:05"><w:p><w:r><w:t>note: fix</w:t></w:r></w:p></w:comment>"#,
                r#"</w:comments>"#,
            );
            let base = minimal_docx(body);
            let mut c = DocxContainer::read(&base).unwrap();
            c.set("word/comments.xml", comments.as_bytes().to_vec());
            let docx = c.write().unwrap();

            let mut d = Document::new();
            import_docx(&docx, &mut d);
            let snap = d.materialize();
            let p = &snap.blocks[0];
            assert_eq!(
                p.text, "fix this word",
                "comment markup not in text: {:?}",
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
                other => panic!("comment value is the comment id: {other:?}"),
            };
            assert_eq!(snap.comments.len(), 1, "one comment: {:?}", snap.comments);
            let cmt = &snap.comments[0];
            assert_eq!(cmt.id, cid);
            assert_eq!(cmt.author, "Ada");
            assert_eq!(cmt.date, "2021-01-02T03:04:05");
            assert_eq!(cmt.blocks[0].text, "note: fix");
        }
        // Phase `comment_roundtrips_through_docx`.
        {
            use tilo_doc::document::MarkValue;
            use tilo_doc::schema::MARK_COMMENT;
            type CommentTuple = (String, String, Option<(usize, usize)>);
            // A comment built from scratch over "this word" must export comments.xml + range markers
            // + reference + content-types/rels, re-importing intact.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "please fix this word now");
            d.add_comment(&p, 11, 20, "Ada Lovelace", "note: fix"); // over "this word"

            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            let cmts =
                std::str::from_utf8(cont.get("word/comments.xml").expect("comments.xml written"))
                    .unwrap();
            assert!(
                cmts.contains(r#"w:author="Ada Lovelace""#),
                "author emitted: {cmts}"
            );
            assert!(cmts.contains("note: fix"), "comment body emitted: {cmts}");
            let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
            assert!(
                document.contains("<w:commentRangeStart"),
                "range start emitted: {document}"
            );
            assert!(
                document.contains("<w:commentRangeEnd"),
                "range end emitted: {document}"
            );
            assert!(
                document.contains("<w:commentReference"),
                "reference emitted: {document}"
            );
            let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
            assert!(
                ct.contains("/word/comments.xml"),
                "comments part in content-types: {ct}"
            );
            let rels =
                std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
            assert!(
                rels.contains(r#"Target="comments.xml""#),
                "comments rel registered: {rels}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
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
            let cmt = snap
                .comments
                .iter()
                .find(|c| c.id == new_id)
                .expect("comment present by id");
            assert_eq!(cmt.author, "Ada Lovelace");
            assert_eq!(cmt.blocks[0].text, "note: fix");
            assert_eq!(cmt.range, Some((snap.blocks[0].id.clone(), 11, 20)));

            let exported2 = export_docx(&d2);
            let mut d3 = Document::new();
            import_docx(&exported2, &mut d3);
            let authors_texts = |snap: &DocSnapshot| {
                let mut v: Vec<CommentTuple> = snap
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
        // Phase `tracked_changes_roundtrip_through_docx`.
        {
            type ChangeTuple = (
                tilo_doc::snapshot::ChangeKind,
                String,
                Option<(usize, usize)>,
            );
            // Tracked insertion + deletion (with author) export as <w:ins>/<w:del> wrappers (deleted
            // text via <w:delText>), re-importing kind/author + anchored ranges.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "hello world");
            let ins = d.track_insert(&p, 5, " there", "Ada", "2021-01-02T00:00:00Z");
            let del = d.track_delete(&p, 12, 17, "Bob", "2021-03-04T00:00:00Z"); // "world"
            let _ = (ins, del);

            let exported = export_docx(&d);
            let cont = DocxContainer::read(&exported).unwrap();
            let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
            assert_docx_shows_tracked_changes(document);

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            let snap = d2.materialize();
            let (ins_id, del_id) = assert_import_recovers_changes(&snap);

            // accept_change on the insertion clears the mark; on the deletion removes text.
            d2.accept_change(&ins_id);
            d2.accept_change(&del_id);
            let after = d2.materialize();
            assert_eq!(
                after.blocks[0].text, "hello there ",
                "accept-ins keeps text; accept-del removes 'world'"
            );
            assert!(after.changes.is_empty(), "both changes resolved");

            let exported2 = export_docx(&d);
            let mut d3 = Document::new();
            import_docx(&exported2, &mut d3);
            let summary = |snap: &DocSnapshot| {
                let mut v: Vec<ChangeTuple> = snap
                    .changes
                    .iter()
                    .map(|c| {
                        (
                            c.kind,
                            c.author.clone(),
                            c.range.as_ref().map(|(_, s, e)| (*s, *e)),
                        )
                    })
                    .collect();
                v.sort_by(|a, b| (a.2, &a.1).cmp(&(b.2, &b.1)));
                v
            };
            let mut d_re = Document::new();
            import_docx(&exported, &mut d_re);
            assert_eq!(
                summary(&d_re.materialize()),
                summary(&d3.materialize()),
                "changes stable across a second round-trip"
            );
        }
    }

    /// Builds the footnote+endnote fixture and exports it to `.docx` bytes
    /// (part of [`footnote_and_endnote_roundtrip_through_docx`]).
    fn export_footnote_endnote_fixture() -> Vec<u8> {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "see ref");
        let footnote_id = d.insert_footnote(&p, 4);
        let fb = d
            .insert_note_block(&footnote_id, None, "paragraph")
            .unwrap();
        d.splice_note_text(&footnote_id, &fb, 0, 0, "a footnote");

        let p2 = d.insert_block(Some(&p), "paragraph");
        d.splice_text(&p2, 0, 0, "tail");
        let endnote_id = d.insert_endnote(&p2, 4);
        let eb = d.insert_note_block(&endnote_id, None, "paragraph").unwrap();
        d.splice_note_text(&endnote_id, &eb, 0, 0, "an endnote");

        export_docx(&d)
    }

    /// Asserts the exported `.docx` carries both note parts, their refs, and
    /// the content-types/rels registrations (part of
    /// [`footnote_and_endnote_roundtrip_through_docx`]).
    fn assert_footnote_endnote_docx_xml(exported: &[u8]) {
        let cont = DocxContainer::read(exported).unwrap();
        let ftn = std::str::from_utf8(
            cont.get("word/footnotes.xml")
                .expect("footnotes.xml written"),
        )
        .unwrap();
        assert!(ftn.contains("a footnote"), "footnote body emitted: {ftn}");
        let end = std::str::from_utf8(cont.get("word/endnotes.xml").expect("endnotes.xml written"))
            .unwrap();
        assert!(end.contains("an endnote"), "endnote body emitted: {end}");
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(
            document.contains("<w:footnoteReference"),
            "footnote reference emitted: {document}"
        );
        assert!(
            document.contains("<w:endnoteReference"),
            "endnote reference emitted: {document}"
        );
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(
            ct.contains("/word/footnotes.xml") && ct.contains("/word/endnotes.xml"),
            "note parts in content-types: {ct}"
        );
        let rels = std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
        assert!(
            rels.contains(r#"Target="footnotes.xml""#) && rels.contains(r#"Target="endnotes.xml""#),
            "note rels registered: {rels}"
        );
    }

    /// Re-imports the exported `.docx`, asserting the ref marker and both
    /// note bodies/kinds survive (part of
    /// [`footnote_and_endnote_roundtrip_through_docx`]). Returns the
    /// re-imported document for the caller's second round-trip check.
    fn assert_footnote_endnote_reimport(exported: &[u8]) -> Document {
        use tilo_doc::schema::MARK_NOTE_REF;

        let mut d2 = Document::new();
        import_docx(exported, &mut d2);
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
            .expect("footnote present by id");
        assert_eq!(foot.kind, "footnote");
        assert_eq!(foot.blocks[0].text, "a footnote");
        let end_note = snap
            .notes
            .iter()
            .find(|n| n.kind == "endnote")
            .expect("endnote present");
        assert_eq!(end_note.blocks[0].text, "an endnote");

        d2
    }

    /// Asserts the exported `document.xml` carries the `<w:ins>`/`<w:del>` wrappers,
    /// `<w:delText>`, and both change authors.
    fn assert_docx_shows_tracked_changes(document: &str) {
        assert!(
            document.contains("<w:ins "),
            "ins wrapper emitted: {document}"
        );
        assert!(
            document.contains("<w:del "),
            "del wrapper emitted: {document}"
        );
        assert!(
            document.contains("<w:delText"),
            "deleted text uses delText: {document}"
        );
        assert!(
            document.contains(r#"w:author="Ada""#),
            "ins author emitted: {document}"
        );
        assert!(
            document.contains(r#"w:author="Bob""#),
            "del author emitted: {document}"
        );
    }

    /// Asserts a re-imported snapshot carries both marks at their original ranges and both
    /// `ChangeView` records with the expected kind/author/range; returns the (ins, del) ids.
    fn assert_import_recovers_changes(snap: &DocSnapshot) -> (String, String) {
        // The deleted text is retained (struck), the inserted text is present.
        assert_eq!(
            snap.blocks[0].text, "hello there world",
            "ins kept, del retained: {:?}",
            snap.blocks[0].text
        );
        let im = snap.blocks[0]
            .marks
            .iter()
            .find(|m| m.name == tilo_doc::schema::MARK_INS)
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
            .find(|m| m.name == tilo_doc::schema::MARK_DEL)
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
        (ins_id, del_id)
    }

    #[test]
    fn section_columns_roundtrip_through_docx() {
        // Phase `columns_roundtrip`.
        {
            // 2-column body with a gap must round-trip; sectPr carries `w:cols w:num="2" w:space="…"`.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "flows across columns");
            d.set_columns(2, Some(24.0)); // 24 px gap → 360 twips

            let exported = export_docx(&d);
            let out = DocxContainer::read(&exported).unwrap();
            let document = std::str::from_utf8(out.get("word/document.xml").unwrap()).unwrap();
            assert!(
                document.contains("<w:sectPr>"),
                "sectPr emitted for columns: {document}"
            );
            assert!(
                document.contains(r#"<w:cols w:num="2""#),
                "w:cols num=2 in sectPr: {document}"
            );
            assert!(
                document.contains(r#"w:space="360""#),
                "w:cols space=360 twips (24px): {document}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            let (count, gap) = d2.columns();
            assert_eq!(count, 2, "column count survives .docx round-trip");
            assert_eq!(gap, Some(24.0), "column gap (px) survives .docx round-trip");
            assert_eq!(
                d2.materialize().columns,
                2,
                "snapshot surfaces the column count"
            );
        }
        // Phase `columns_added_to_existing_header_sectpr`.
        {
            // A doc that already needs a sectPr (has a header) must get w:cols ADDED to it, not a second one.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "body");
            let h = d.insert_header_block(None, "paragraph");
            d.splice_header_text(&h, 0, 0, "Header");
            d.set_columns(3, None);

            let exported = export_docx(&d);
            let out = DocxContainer::read(&exported).unwrap();
            let document = std::str::from_utf8(out.get("word/document.xml").unwrap()).unwrap();
            assert_eq!(
                document.matches("<w:sectPr>").count(),
                1,
                "exactly one sectPr: {document}"
            );
            assert!(
                document.contains("<w:headerReference"),
                "header reference kept: {document}"
            );
            assert!(
                document.contains(r#"<w:cols w:num="3""#),
                "columns folded into the sectPr: {document}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            assert_eq!(
                d2.columns().0,
                3,
                "column count survives alongside the header"
            );
            assert_eq!(
                d2.header()[0].text,
                "Header",
                "header survives alongside columns"
            );
        }
        // Phase `single_column_emits_no_cols`.
        {
            // Default single-column doc must not emit w:cols, nor force a sectPr for nothing.
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "plain");

            let exported = export_docx(&d);
            let out = DocxContainer::read(&exported).unwrap();
            let document = std::str::from_utf8(out.get("word/document.xml").unwrap()).unwrap();
            assert!(
                !document.contains("<w:cols"),
                "no w:cols for a single-column doc: {document}"
            );

            let mut d2 = Document::new();
            import_docx(&exported, &mut d2);
            assert_eq!(d2.columns(), (1, None), "single-column default round-trips");
        }
    }

    #[test]
    fn keeps_paragraph_properties_links_merged_cells_and_numbering() {
        let numbering = concat!(
            r#"<w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1)"/></w:lvl></w:abstractNum>"#,
            r#"<w:num w:numId="9"><w:abstractNumId w:val="7"/></w:num>"#,
        );
        let body = concat!(
            r#"<w:p><w:pPr><w:keepNext/><w:spacing w:before="120" w:line="360" w:lineRule="auto"/><w:ind w:start="720"/></w:pPr>"#,
            r#"<w:hyperlink r:id="rIdL"><w:r><w:t>site</w:t></w:r></w:hyperlink></w:p>"#,
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="9"/></w:numPr></w:pPr><w:r><w:t>item</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:sectPr><w:cols w:num="2"/></w:sectPr></w:pPr></w:p>"#,
            r#"<w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge w:val="restart"/></w:tcPr><w:p><w:r><w:t>big</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:p><w:r><w:t>c</w:t></w:r></w:p></w:tc></w:tr>"#,
            r#"<w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:vMerge/></w:tcPr><w:p/></w:tc>"#,
            r#"<w:tc><w:p><w:r><w:t>f</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        );
        let mut c = DocxContainer::read(&super::super::container::minimal_docx_numbered(
            "", numbering, body,
        ))
        .unwrap();
        let rels = String::from_utf8(c.get("word/_rels/document.xml.rels").unwrap().to_vec())
            .unwrap()
            .replace(
                "</Relationships>",
                r#"<Relationship Id="rIdL" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org/?a=1&amp;b=2" TargetMode="External"/></Relationships>"#,
            );
        c.set("word/_rels/document.xml.rels", rels.into_bytes());
        let mut d = Document::new();
        DocxCodec
            .import(
                &c.write().unwrap(),
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        let snap = d.materialize();
        assert!(
            snap.blocks[0].marks.iter().any(|m| m.name == "link"
                && m.value == MarkValue::Str("https://example.org/?a=1&b=2".into())),
            "{:?}",
            snap.blocks[0].marks
        );
        assert_eq!(snap.blocks[1].attrs.get("level"), Some(&AttrVal::Int(1)));
        let big = snap.blocks.iter().find(|b| b.text == "big").unwrap();
        assert_eq!(big.attrs.get("colspan"), Some(&AttrVal::Int(2)));
        assert_eq!(big.attrs.get("rowspan"), Some(&AttrVal::Int(2)));
        let f = snap.blocks.iter().find(|b| b.text == "f").unwrap();
        assert_eq!(f.attrs.get("col"), Some(&AttrVal::Int(2)), "{:?}", f.attrs);

        let out = DocxCodec
            .export(d.as_ref(), &tilo_content::NoContent)
            .unwrap();
        let c = DocxContainer::read(&out).unwrap();
        let text = |part: &str| String::from_utf8(c.get(part).unwrap().to_vec()).unwrap();
        let xml = text("word/document.xml");
        for kept in [
            r#"<w:pPr><w:keepNext/><w:spacing w:before="120" w:line="360" w:lineRule="auto"/><w:ind w:start="720"/></w:pPr>"#,
            r#"<w:numPr><w:ilvl w:val="1"/><w:numId w:val="9"/></w:numPr>"#,
            r#"<w:sectPr><w:cols w:num="2"/></w:sectPr>"#,
            r#"<w:gridSpan w:val="2"/><w:vMerge w:val="restart"/>"#,
            r#"<w:gridSpan w:val="2"/><w:vMerge/>"#,
            r#"<w:hyperlink r:id="rIdQdocLink1">"#,
        ] {
            assert!(xml.contains(kept), "{kept} in {xml}");
        }
        assert!(text("word/_rels/document.xml.rels").contains(
            r#"Id="rIdQdocLink1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org/?a=1&amp;b=2" TargetMode="External""#
        ));
        // The imported numbering still numbers the item, so it is kept as it was.
        assert!(text("word/numbering.xml").contains(r#"<w:lvlText w:val="%1)"/>"#));

        let mut again = Document::new();
        DocxCodec
            .import(
                &out,
                &mut again.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
        assert_eq!(norm(&again.materialize()), norm(&snap));
    }
}
