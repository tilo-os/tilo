//! Export the canonical model back to a `.docx`. Generates a minimal valid
//! `word/document.xml` from the model with the inverse mapping, re-emits every
//! preserved part verbatim, and synthesises `[Content_Types].xml` / `_rels/.rels`
//! / `word/_rels/document.xml.rels` / `word/numbering.xml` when the source had
//! none (e.g. a doc created from scratch rather than imported).

use super::container::DocxContainer;
use crate::document::MarkValue;
use crate::format::CodecError;
use crate::model::DocRef;
use crate::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME, STYLE_SIZE, STYLE_SPACE_AFTER,
    STYLE_SPACE_BEFORE,
};
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use std::collections::{BTreeMap, BTreeSet};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

const CONTENT_TYPES_BASE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const NUMBERING_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#;

const NUMBERING_OVERRIDE: &str = r#"<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>"#;
const NUMBERING_REL: &str = r#"<Relationship Id="rIdQdocNum" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#;

const CORE_OVERRIDE: &str = r#"<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>"#;
const APP_OVERRIDE: &str = r#"<Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#;
const CORE_REL: &str = r#"<Relationship Id="rIdQdocCore" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>"#;
const APP_REL: &str = r#"<Relationship Id="rIdQdocApp" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/>"#;

const HEADER_OVERRIDE: &str = r#"<Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#;
const FOOTER_OVERRIDE: &str = r#"<Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/>"#;
const HEADER_REL: &str = r#"<Relationship Id="rIdQdocHdr" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>"#;
const FOOTER_REL: &str = r#"<Relationship Id="rIdQdocFtr" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/>"#;

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

pub(crate) fn export(doc: DocRef) -> Result<Vec<u8>, CodecError> {
    let snap = doc.materialize();
    // Page header/footer parts + the `w:sectPr` references that wire them in.
    let hf_parts = super::headers::export_parts(&doc);
    // Multi-column section layout: a `<w:cols>` element folded into the same sectPr.
    let cols = super::columns::section_columns_xml(&doc).unwrap_or_default();
    // The `w:sectPr` is emitted when *either* the header/footer references or the
    // columns need to ride in it. In a sectPr's defined element order the
    // header/footer references precede `w:cols`, so the references come first.
    let sectpr = if hf_parts.any() || !cols.is_empty() {
        format!(r#"<w:sectPr>{}{}</w:sectPr>"#, super::headers::section_references(&hf_parts), cols)
    } else {
        String::new()
    };
    // Footnote/endnote parts + the qdoc-id → docx-id / kind maps the body references.
    let notes = super::notes::export_notes(&doc);
    // Comments part + the qdoc-id → docx-id map the body's range markers reference.
    let comments = super::comments::export_comments(&doc);
    let (document, named_refs, numbering, images) =
        build_document_xml(&snap, &sectpr, &notes.id_map, &notes.kind_map, &comments.id_map);
    let has_lists = snap.blocks.iter().any(|b| b.kind == "list_item");
    let has_styles = !named_refs.is_empty();
    let has_images = !images.images.is_empty();

    let mut c = DocxContainer { parts: Vec::new() };
    // Re-emit every preserved part verbatim (styles.xml, settings.xml, the
    // original content-types/rels, images, …).
    let mut had_content_types = false;
    let mut had_root_rels = false;
    let mut had_doc_rels = false;
    let mut had_numbering = false;
    let mut had_styles_part = false;
    let mut had_core = false;
    let mut had_app = false;
    let mut had_header_part = false;
    let mut had_footer_part = false;
    let mut had_footnotes_part = false;
    let mut had_endnotes_part = false;
    let mut had_comments_part = false;
    let mut original_core: Option<Vec<u8>> = None;
    let mut original_app: Option<Vec<u8>> = None;
    for (key, bytes) in crate::model::preserved_parts(doc.doc, doc.c) {
        if let Some(name) = key.strip_prefix("docx/part/") {
            had_content_types |= name == "[Content_Types].xml";
            had_root_rels |= name == "_rels/.rels";
            had_doc_rels |= name == "word/_rels/document.xml.rels";
            had_numbering |= name == "word/numbering.xml";
            had_styles_part |= name == "word/styles.xml";
            had_header_part |= name == super::headers::HEADER_PART;
            had_footer_part |= name == super::headers::FOOTER_PART;
            had_footnotes_part |= name == super::notes::FOOTNOTES_PART;
            had_endnotes_part |= name == super::notes::ENDNOTES_PART;
            had_comments_part |= name == super::comments::COMMENTS_PART;
            if name == "docProps/core.xml" {
                had_core = true;
                original_core = Some(bytes.clone());
            }
            if name == "docProps/app.xml" {
                had_app = true;
                original_app = Some(bytes.clone());
            }
            c.set(name, bytes);
        }
    }

    c.set("word/document.xml", document.into_bytes());

    // Image media parts. Each exported drawing references a `word/media/imageN.<ext>`
    // part by a synthesised relationship; the bytes are written here.
    for img in &images.images {
        c.set(&img.part, img.bytes.clone());
    }

    // Page header/footer parts. Written under the fixed `word/header1.xml` /
    // `word/footer1.xml` names (the `w:sectPr` built above references them by the
    // fixed rel ids), overwriting a same-named preserved part. A part newly created
    // here needs a content-type override + a document relationship.
    if let Some(hdr) = hf_parts.header.clone() {
        c.set(super::headers::HEADER_PART, hdr);
    }
    if let Some(ftr) = hf_parts.footer.clone() {
        c.set(super::headers::FOOTER_PART, ftr);
    }
    let need_header_wireup = hf_parts.header.is_some() && !had_header_part;
    let need_footer_wireup = hf_parts.footer.is_some() && !had_footer_part;

    // Footnote/endnote parts. Written under the fixed `word/footnotes.xml` /
    // `word/endnotes.xml` names; the body references emitted above use the matching
    // docx ids. A part newly created here needs a content-type override + a document
    // relationship.
    if let Some(ftn) = notes.footnotes_xml.clone() {
        c.set(super::notes::FOOTNOTES_PART, ftn);
    }
    if let Some(end) = notes.endnotes_xml.clone() {
        c.set(super::notes::ENDNOTES_PART, end);
    }
    let need_footnotes_wireup = notes.footnotes_xml.is_some() && !had_footnotes_part;
    let need_endnotes_wireup = notes.endnotes_xml.is_some() && !had_endnotes_part;

    // Comments part. Written under the fixed `word/comments.xml` name; the body's
    // `w:commentRangeStart/End` + `w:commentReference` use the matching docx ids. A
    // part newly created here needs a content-type override + a document relationship.
    if let Some(cmt) = comments.comments_xml.clone() {
        c.set(super::comments::COMMENTS_PART, cmt);
    }
    let need_comments_wireup = comments.comments_xml.is_some() && !had_comments_part;

    // Document properties → `docProps/core.xml` (+ `app.xml` generator). Regenerate
    // from the canonical properties, merging unmodeled children of the preserved
    // originals. A part newly created here needs a content-type override + a root
    // relationship (an imported docx already has both, so only synthesised parts
    // are wired up).
    let wrote_core = if let Some(core_xml) = super::meta::export_core(&doc, original_core.as_deref()) {
        c.set("docProps/core.xml", core_xml);
        true
    } else {
        false
    };
    let wrote_app = if let Some(app_xml) = super::meta::export_app(&doc, original_app.as_deref()) {
        c.set("docProps/app.xml", app_xml);
        true
    } else {
        false
    };
    let need_core_wireup = wrote_core && !had_core;
    let need_app_wireup = wrote_app && !had_app;

    if has_lists {
        // Ensure a numbering part exists mapping numId 1 → bullet, 2 → decimal, plus
        // any custom ordered numberings (roman/alpha and/or non-1 start) the
        // document uses, so list numbering format + start round-trip. The static
        // fallback covers the common bullet/decimal case.
        let xml = if numbering.has_custom() { numbering.numbering_xml() } else { NUMBERING_XML.to_string() };
        c.set("word/numbering.xml", xml.into_bytes());
    }

    // word/styles.xml — merge the referenced CRDT styles into the preserved part
    // (or synthesise a fresh one) as `w:style w:type="paragraph"` definitions so
    // Word/LibreOffice render the document's named styles. Styles already declared
    // in a preserved styles.xml are referenced, not redefined.
    if has_styles {
        let merged = merge_styles_xml(c.get("word/styles.xml"), &named_refs, &snap.styles);
        c.set("word/styles.xml", merged);
    }
    // A styles part referenced for the first time still needs a content-type
    // override + relationship (an imported docx already has both).
    let need_styles_wireup = has_styles && !had_styles_part;

    // [Content_Types].xml — generate when absent; otherwise ensure overrides.
    if !had_content_types {
        let mut ct = CONTENT_TYPES_BASE.to_string();
        if has_lists {
            ct.push_str(NUMBERING_OVERRIDE);
        }
        if has_styles {
            ct.push_str(STYLES_OVERRIDE);
        }
        if wrote_core {
            ct.push_str(CORE_OVERRIDE);
        }
        if wrote_app {
            ct.push_str(APP_OVERRIDE);
        }
        if hf_parts.header.is_some() {
            ct.push_str(HEADER_OVERRIDE);
        }
        if hf_parts.footer.is_some() {
            ct.push_str(FOOTER_OVERRIDE);
        }
        if notes.footnotes_xml.is_some() {
            ct.push_str(FOOTNOTES_OVERRIDE);
        }
        if notes.endnotes_xml.is_some() {
            ct.push_str(ENDNOTES_OVERRIDE);
        }
        if comments.comments_xml.is_some() {
            ct.push_str(COMMENTS_OVERRIDE);
        }
        for def in images.content_type_defaults() {
            ct.push_str(&def);
        }
        ct.push_str("</Types>");
        c.set("[Content_Types].xml", ct.into_bytes());
    } else {
        let mut overrides: Vec<(&str, &str)> = Vec::new();
        if has_lists && !had_numbering {
            overrides.push(("/word/numbering.xml", NUMBERING_OVERRIDE));
        }
        if need_styles_wireup {
            overrides.push(("/word/styles.xml", STYLES_OVERRIDE));
        }
        if need_core_wireup {
            overrides.push(("/docProps/core.xml", CORE_OVERRIDE));
        }
        if need_app_wireup {
            overrides.push(("/docProps/app.xml", APP_OVERRIDE));
        }
        if need_header_wireup {
            overrides.push(("/word/header1.xml", HEADER_OVERRIDE));
        }
        if need_footer_wireup {
            overrides.push(("/word/footer1.xml", FOOTER_OVERRIDE));
        }
        if need_footnotes_wireup {
            overrides.push(("/word/footnotes.xml", FOOTNOTES_OVERRIDE));
        }
        if need_endnotes_wireup {
            overrides.push(("/word/endnotes.xml", ENDNOTES_OVERRIDE));
        }
        if need_comments_wireup {
            overrides.push(("/word/comments.xml", COMMENTS_OVERRIDE));
        }
        let image_defaults = images.content_type_defaults();
        if !overrides.is_empty() || !image_defaults.is_empty() {
            if let Some(ct) = c.get("[Content_Types].xml") {
                let mut s = String::from_utf8_lossy(ct).into_owned();
                for (part, ovr) in overrides {
                    if !s.contains(part) {
                        inject_before(&mut s, "</Types>", ovr);
                    }
                }
                // Image content-type `Default`s — dedup on `Extension="<ext>"`.
                for img in &images.images {
                    let probe = format!(r#"Extension="{}""#, img.ext);
                    if !s.contains(&probe) {
                        let (_, media) = image_ext_and_media(&img.bytes);
                        let def = format!(r#"<Default Extension="{}" ContentType="{}"/>"#, img.ext, media);
                        inject_before(&mut s, "</Types>", &def);
                    }
                }
                c.set("[Content_Types].xml", s.into_bytes());
            }
        }
    }

    // _rels/.rels — the docProps relationships live in the *root* rels (not the
    // document's). Generate fresh when absent; otherwise inject any newly-created
    // docProps relationships.
    if !had_root_rels {
        let mut s = ROOT_RELS.to_string();
        if wrote_core {
            inject_before(&mut s, "</Relationships>", CORE_REL);
        }
        if wrote_app {
            inject_before(&mut s, "</Relationships>", APP_REL);
        }
        c.set("_rels/.rels", s.into_bytes());
    } else if need_core_wireup || need_app_wireup {
        if let Some(rels) = c.get("_rels/.rels") {
            let mut s = String::from_utf8_lossy(rels).into_owned();
            if need_core_wireup && !s.contains("Target=\"docProps/core.xml\"") {
                inject_before(&mut s, "</Relationships>", CORE_REL);
            }
            if need_app_wireup && !s.contains("Target=\"docProps/app.xml\"") {
                inject_before(&mut s, "</Relationships>", APP_REL);
            }
            c.set("_rels/.rels", s.into_bytes());
        }
    }

    // word/_rels/document.xml.rels — needed once there's a numbering, a
    // freshly-synthesised styles part, a header/footer part, and/or a notes part.
    if has_lists
        || need_styles_wireup
        || need_header_wireup
        || need_footer_wireup
        || need_footnotes_wireup
        || need_endnotes_wireup
        || need_comments_wireup
        || has_images
    {
        if !had_doc_rels {
            let mut inner = String::new();
            if need_styles_wireup {
                inner.push_str(STYLES_REL);
            }
            if has_lists {
                inner.push_str(NUMBERING_REL);
            }
            if need_header_wireup {
                inner.push_str(HEADER_REL);
            }
            if need_footer_wireup {
                inner.push_str(FOOTER_REL);
            }
            if need_footnotes_wireup {
                inner.push_str(FOOTNOTES_REL);
            }
            if need_endnotes_wireup {
                inner.push_str(ENDNOTES_REL);
            }
            if need_comments_wireup {
                inner.push_str(COMMENTS_REL);
            }
            inner.push_str(&images.relationships());
            let rels = format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{inner}</Relationships>"#
            );
            c.set("word/_rels/document.xml.rels", rels.into_bytes());
        } else if let Some(rels) = c.get("word/_rels/document.xml.rels") {
            let mut s = String::from_utf8_lossy(rels).into_owned();
            if has_lists && !s.contains("Target=\"numbering.xml\"") {
                inject_before(&mut s, "</Relationships>", NUMBERING_REL);
            }
            if need_styles_wireup && !s.contains("Target=\"styles.xml\"") {
                inject_before(&mut s, "</Relationships>", STYLES_REL);
            }
            if need_header_wireup && !s.contains("Target=\"header1.xml\"") {
                inject_before(&mut s, "</Relationships>", HEADER_REL);
            }
            if need_footer_wireup && !s.contains("Target=\"footer1.xml\"") {
                inject_before(&mut s, "</Relationships>", FOOTER_REL);
            }
            if need_footnotes_wireup && !s.contains("Target=\"footnotes.xml\"") {
                inject_before(&mut s, "</Relationships>", FOOTNOTES_REL);
            }
            if need_endnotes_wireup && !s.contains("Target=\"endnotes.xml\"") {
                inject_before(&mut s, "</Relationships>", ENDNOTES_REL);
            }
            if need_comments_wireup && !s.contains("Target=\"comments.xml\"") {
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

    c.write()
}

/// Build or extend `word/styles.xml` with `<w:style w:type="paragraph">` defs for
/// the referenced container styles. A style already declared (by `w:styleId`) in a
/// preserved styles.xml is left untouched (no duplicate; the preserved bytes win).
fn merge_styles_xml(existing: Option<&[u8]>, refs: &BTreeSet<String>, styles: &BTreeMap<String, BTreeMap<String, AttrVal>>) -> Vec<u8> {
    match existing {
        Some(bytes) => {
            let present = style_ids_in(bytes);
            let mut defs = String::new();
            for id in refs {
                if present.contains(id) {
                    continue;
                }
                if let Some(props) = styles.get(id) {
                    defs.push_str(&para_style_def_xml(id, props));
                }
            }
            if defs.is_empty() {
                return bytes.to_vec();
            }
            let mut s = String::from_utf8_lossy(bytes).into_owned();
            inject_before(&mut s, "</w:styles>", &defs);
            s.into_bytes()
        }
        None => {
            let mut s = STYLES_XML_OPEN.to_string();
            for id in refs {
                if let Some(props) = styles.get(id) {
                    s.push_str(&para_style_def_xml(id, props));
                }
            }
            s.push_str("</w:styles>");
            s.into_bytes()
        }
    }
}

/// The `w:styleId` values declared in a styles.xml, so existing styles aren't
/// redefined when merging.
fn style_ids_in(xml: &[u8]) -> BTreeSet<String> {
    let s = String::from_utf8_lossy(xml);
    let mut out = BTreeSet::new();
    let needle = "w:styleId=\"";
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

/// An OOXML `<w:style w:type="paragraph">` definition for a CRDT styles-container
/// style (`w:pPr` alignment/spacing/indent + `w:rPr` font/bold/italic/colour/size),
/// the inverse of `styles::parse_styles` + `styles::style_props`.
fn para_style_def_xml(id: &str, props: &BTreeMap<String, AttrVal>) -> String {
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

    let mut pp = String::new();
    if let Some(a) = gets(STYLE_ALIGN) {
        let v = match a {
            "center" => "center",
            "right" => "right",
            "justify" => "both",
            _ => "",
        };
        if !v.is_empty() {
            pp.push_str(&format!(r#"<w:jc w:val="{v}"/>"#));
        }
    }
    let before = getf(STYLE_SPACE_BEFORE).filter(|v| *v != 0.0);
    let after = getf(STYLE_SPACE_AFTER).filter(|v| *v != 0.0);
    if before.is_some() || after.is_some() {
        pp.push_str("<w:spacing");
        if let Some(v) = before {
            pp.push_str(&format!(r#" w:before="{}""#, px_to_twips(v)));
        }
        if let Some(v) = after {
            pp.push_str(&format!(r#" w:after="{}""#, px_to_twips(v)));
        }
        pp.push_str("/>");
    }
    if let Some(v) = getf(STYLE_INDENT).filter(|v| *v != 0.0) {
        pp.push_str(&format!(r#"<w:ind w:left="{}"/>"#, px_to_twips(v)));
    }

    let mut rp = String::new();
    if let Some(f) = gets(STYLE_FONT) {
        let n = font_name(f);
        rp.push_str(&format!(r#"<w:rFonts w:ascii="{n}" w:hAnsi="{n}"/>"#));
    }
    if getb(STYLE_BOLD) {
        rp.push_str("<w:b/>");
    }
    if getb(STYLE_ITALIC) {
        rp.push_str("<w:i/>");
    }
    if let Some(c) = gets(STYLE_COLOR) {
        rp.push_str(&format!(r#"<w:color w:val="{}"/>"#, color_hex(c)));
    }
    if let Some(sz) = getf(STYLE_SIZE) {
        let hp = (sz * 2.0).round() as i64;
        rp.push_str(&format!(r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#));
    }

    let name = gets(STYLE_NAME).unwrap_or(id);
    let ppr = if pp.is_empty() { String::new() } else { format!("<w:pPr>{pp}</w:pPr>") };
    let rpr = if rp.is_empty() { String::new() } else { format!("<w:rPr>{rp}</w:rPr>") };
    format!(
        r#"<w:style w:type="paragraph" w:styleId="{}"><w:name w:val="{}"/>{ppr}{rpr}</w:style>"#,
        escape_attr(id),
        escape_attr(name)
    )
}

/// Pixels (96 dpi) → OOXML twips (1/1440 inch): the inverse of
/// `styles::twips_to_px`. 1 px = 15 twips.
fn px_to_twips(px: f64) -> i64 {
    (px * 15.0).round() as i64
}

/// `#rrggbb` → OOXML `RRGGBB` (no hash, upper-case as Word writes it). Re-imports
/// to the same canonical `#rrggbb` via `styles::normalize_color`.
fn color_hex(c: &str) -> String {
    c.trim_start_matches('#').to_ascii_uppercase()
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

/// Insert `frag` immediately before the first occurrence of `needle`.
fn inject_before(s: &mut String, needle: &str, frag: &str) {
    if let Some(pos) = s.rfind(needle) {
        s.insert_str(pos, frag);
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

/// Concrete font name for a generic family (round-trips via `generic_font`).
fn font_name(generic: &str) -> &'static str {
    match generic {
        "serif" => "Times New Roman",
        "mono" => "Courier New",
        _ => "Arial",
    }
}

/// The active marks covering character `i`, sorted by name for stable grouping.
fn marks_at(i: usize, marks: &[crate::document::MarkSpan]) -> Vec<(String, MarkValue)> {
    let mut v: Vec<(String, MarkValue)> = marks
        .iter()
        .filter(|m| i >= m.start && i < m.end)
        .map(|m| (m.name.clone(), m.value.clone()))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

/// Build the `<w:rPr>` body for an active mark set (empty if none modeled).
fn run_props(marks: &[(String, MarkValue)]) -> String {
    let (mut b, mut i, mut u, mut strike) = (false, false, false, false);
    let mut sz: Option<i64> = None;
    let mut font: Option<&str> = None;
    let mut color: Option<&str> = None;
    let mut highlight: Option<&str> = None;
    // Vertical alignment: `superscript`/`subscript` (super wins if both present).
    let mut vert_align: Option<&'static str> = None;
    for (name, val) in marks {
        match name.as_str() {
            "strong" => b = true,
            "em" => i = true,
            "underline" => u = true,
            "strike" => strike = true,
            "fontSize" => sz = val.as_int(),
            "fontFamily" => font = val.as_str(),
            "color" => color = val.as_str(),
            "highlight" => highlight = val.as_str(),
            "super" => vert_align = Some("superscript"),
            "sub" if vert_align.is_none() => vert_align = Some("subscript"),
            _ => {}
        }
    }
    let mut s = String::new();
    if let Some(f) = font {
        let n = font_name(f);
        s.push_str(&format!(r#"<w:rFonts w:ascii="{n}" w:hAnsi="{n}"/>"#));
    }
    if b {
        s.push_str("<w:b/>");
    }
    if i {
        s.push_str("<w:i/>");
    }
    if strike {
        s.push_str("<w:strike/>");
    }
    if let Some(c) = color {
        s.push_str(&format!(r#"<w:color w:val="{}"/>"#, color_hex(c)));
    }
    // Text background (highlight): `w:shd w:fill` carries the arbitrary hex
    // (re-imported via `w:shd`), which round-trips any colour (unlike the named
    // `w:highlight` palette).
    if let Some(c) = highlight {
        s.push_str(&format!(r#"<w:shd w:val="clear" w:color="auto" w:fill="{}"/>"#, color_hex(c)));
    }
    if let Some(n) = sz {
        let hp = n * 2;
        s.push_str(&format!(r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#));
    }
    if u {
        s.push_str(r#"<w:u w:val="single"/>"#);
    }
    if let Some(va) = vert_align {
        s.push_str(&format!(r#"<w:vertAlign w:val="{va}"/>"#));
    }
    s
}

/// Emit run content, turning `\t` → `<w:tab/>` and `\n` → `<w:br/>`.
fn run_content(chars: &[char]) -> String {
    let mut out = String::new();
    let mut buf = String::new();
    let flush = |out: &mut String, buf: &mut String| {
        if !buf.is_empty() {
            out.push_str(r#"<w:t xml:space="preserve">"#);
            for c in buf.chars() {
                push_escaped(out, c);
            }
            out.push_str("</w:t>");
            buf.clear();
        }
    };
    for &c in chars {
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

/// The note id of a `note-ref` mark covering exactly `[i, i+1)`, if any.
fn note_ref_at(marks: &[crate::document::MarkSpan], i: usize) -> Option<String> {
    marks
        .iter()
        .find(|m| m.name == crate::schema::MARK_NOTE_REF && m.start == i && m.end == i + 1)
        .and_then(|m| m.value.as_str().map(|s| s.to_string()))
}

/// The `w:footnoteReference`/`w:endnoteReference` run for a note id, or `None` when
/// the note isn't being exported. Carries the FootnoteReference/EndnoteReference
/// character style so Word superscripts the citation.
fn note_reference_run(
    note_id: &str,
    note_ids: &BTreeMap<String, i64>,
    note_kinds: &BTreeMap<String, crate::model::NoteKind>,
) -> Option<String> {
    let docx_id = note_ids.get(note_id)?;
    let kind = note_kinds.get(note_id).copied().unwrap_or(crate::model::NoteKind::Footnote);
    let (style, elem) = match kind {
        crate::model::NoteKind::Footnote => ("FootnoteReference", "w:footnoteReference"),
        crate::model::NoteKind::Endnote => ("EndnoteReference", "w:endnoteReference"),
    };
    Some(format!(
        r#"<w:r><w:rPr><w:rStyle w:val="{style}"/></w:rPr><{elem} w:id="{docx_id}"/></w:r>"#
    ))
}

/// The comment ids whose `comment` mark starts / ends exactly at `i` (sorted for
/// deterministic output). `ends_at` matches `mark.end == i`.
fn comment_starts_at(marks: &[crate::document::MarkSpan], i: usize) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .filter(|m| m.name == crate::schema::MARK_COMMENT && m.start == i)
        .filter_map(|m| m.value.as_str().map(|s| s.to_string()))
        .collect();
    v.sort();
    v
}

fn comment_ends_at(marks: &[crate::document::MarkSpan], i: usize) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .filter(|m| m.name == crate::schema::MARK_COMMENT && m.end == i)
        .filter_map(|m| m.value.as_str().map(|s| s.to_string()))
        .collect();
    v.sort();
    v
}

/// The bookmark names whose `bookmark` mark starts / ends exactly at `i` (sorted for
/// deterministic output). `ends_at` matches `mark.end == i`.
fn bookmark_starts_at(marks: &[crate::document::MarkSpan], i: usize) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .filter(|m| m.name == crate::schema::MARK_BOOKMARK && m.start == i)
        .filter_map(|m| m.value.as_str().map(|s| s.to_string()))
        .collect();
    v.sort();
    v
}

fn bookmark_ends_at(marks: &[crate::document::MarkSpan], i: usize) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .filter(|m| m.name == crate::schema::MARK_BOOKMARK && m.end == i)
        .filter_map(|m| m.value.as_str().map(|s| s.to_string()))
        .collect();
    v.sort();
    v
}

/// The internal cross-reference target (`#name` → `name`) of a `link` mark covering
/// `[i, i+1)`, if the link is an internal reference (value starts with `#`). External
/// URL links (no `#`) are not modeled in the `.docx` codec and return `None`.
fn cross_ref_at(marks: &[crate::document::MarkSpan], i: usize) -> Option<String> {
    marks
        .iter()
        .find(|m| m.name == crate::schema::MARK_LINK && i >= m.start && i < m.end)
        .and_then(|m| m.value.as_str())
        .and_then(|v| v.strip_prefix(crate::schema::CROSS_REF_PREFIX).map(|s| s.to_string()))
}

/// Whether any run boundary (mark-set change, note-ref placeholder, comment range
/// boundary, bookmark boundary, or cross-reference change) occurs *at* `j`, so a text
/// run must break before it.
fn run_breaks_at(
    chars: &[char],
    marks: &[crate::document::MarkSpan],
    active: &[(String, MarkValue)],
    active_change: &Option<(crate::snapshot::ChangeKind, String)>,
    active_xref: &Option<String>,
    j: usize,
) -> bool {
    marks_at(j, marks) != active
        || &change_at(marks, j) != active_change
        || &cross_ref_at(marks, j) != active_xref
        || is_note_ref_placeholder(chars, marks, j)
        || !comment_starts_at(marks, j).is_empty()
        || !comment_ends_at(marks, j).is_empty()
        || !bookmark_starts_at(marks, j).is_empty()
        || !bookmark_ends_at(marks, j).is_empty()
}

/// A change-id → `&ChangeView` lookup for wrapping runs in `<w:ins>`/`<w:del>` on export.
type ChangeIndex<'a> = BTreeMap<String, &'a crate::snapshot::ChangeView>;

fn change_index(snap: &DocSnapshot) -> ChangeIndex<'_> {
    snap.changes.iter().map(|c| (c.id.clone(), c)).collect()
}

/// The tracked change (kind + id) anchoring char `i`, if any — the `ins`/`del` mark
/// covering `[i, i+1)`. Insertion takes precedence if both somehow overlap (a single
/// char is at most one kind of tracked change in practice).
fn change_at(marks: &[crate::document::MarkSpan], i: usize) -> Option<(crate::snapshot::ChangeKind, String)> {
    let find = |name: &str| {
        marks
            .iter()
            .find(|m| m.name == name && i >= m.start && i < m.end)
            .and_then(|m| m.value.as_str().map(|s| s.to_string()))
    };
    if let Some(id) = find(crate::schema::MARK_INS) {
        return Some((crate::snapshot::ChangeKind::Ins, id));
    }
    find(crate::schema::MARK_DEL).map(|id| (crate::snapshot::ChangeKind::Del, id))
}

/// The `<w:ins …>`/`<w:del …>` open tag for a tracked change, carrying its docx id +
/// author/date attributes. The element wraps the affected `<w:r>` runs.
fn change_open_tag(change: &crate::snapshot::ChangeView, docx_id: i64) -> String {
    let elem = match change.kind {
        crate::snapshot::ChangeKind::Ins => "w:ins",
        crate::snapshot::ChangeKind::Del => "w:del",
    };
    let date = if change.date.is_empty() { String::new() } else { format!(r#" w:date="{}""#, escape_attr(&change.date)) };
    format!(r#"<{elem} w:id="{docx_id}" w:author="{}"{date}>"#, escape_attr(&change.author))
}

/// Emit run content for a *deleted* run, turning text into `<w:delText>` (OOXML requires
/// deleted text use `w:delText`, not `w:t`); `\t`/`\n` still map to `<w:tab/>`/`<w:br/>`.
fn del_run_content(chars: &[char]) -> String {
    let mut out = String::new();
    let mut buf = String::new();
    let flush = |out: &mut String, buf: &mut String| {
        if !buf.is_empty() {
            out.push_str(r#"<w:delText xml:space="preserve">"#);
            for c in buf.chars() {
                push_escaped(out, c);
            }
            out.push_str("</w:delText>");
            buf.clear();
        }
    };
    for &c in chars {
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

/// Render a block's text + marks as a sequence of `<w:r>` runs. A new run starts
/// whenever the active mark set changes, so output is always well-formed. A
/// placeholder carrying a `note-ref` mark is emitted as its reference element run. A
/// `comment` mark emits a `w:commentRangeStart` at its start and a
/// `w:commentRangeEnd` + `w:commentReference` run at its end. A run covered by a
/// tracked `ins`/`del` mark is wrapped in a `<w:ins>`/`<w:del>` element (carrying the
/// change's docx id + author/date); deleted runs use `<w:delText>` for their text.
fn render_runs(
    text: &str,
    marks: &[crate::document::MarkSpan],
    note_ids: &BTreeMap<String, i64>,
    note_kinds: &BTreeMap<String, crate::model::NoteKind>,
    comment_ids: &BTreeMap<String, i64>,
    changes: &ChangeIndex,
    change_ids: &BTreeMap<String, i64>,
    bookmark_ids: &BTreeMap<String, i64>,
) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    // Comment boundary markers at index `i`: ends (+ their reference run) before
    // starts so an adjacent comment's end closes before the next opens.
    let emit_comment_boundaries = |out: &mut String, i: usize| {
        for id in comment_ends_at(marks, i) {
            if let Some(end) = super::comments::comment_range_end(&id, comment_ids) {
                out.push_str(&end);
            }
            if let Some(rf) = super::comments::comment_reference_run(&id, comment_ids) {
                out.push_str(&rf);
            }
        }
        for id in comment_starts_at(marks, i) {
            if let Some(start) = super::comments::comment_range_start(&id, comment_ids) {
                out.push_str(&start);
            }
        }
    };
    // Bookmark boundary markers at index `i`: `w:bookmarkEnd` (by id) before
    // `w:bookmarkStart` (id + name) so an adjacent bookmark's end closes before the
    // next opens. These are empty elements sitting between runs at body level.
    let emit_bookmark_boundaries = |out: &mut String, i: usize| {
        for name in bookmark_ends_at(marks, i) {
            if let Some(id) = bookmark_ids.get(&name) {
                out.push_str(&format!(r#"<w:bookmarkEnd w:id="{id}"/>"#));
            }
        }
        for name in bookmark_starts_at(marks, i) {
            if let Some(id) = bookmark_ids.get(&name) {
                out.push_str(&format!(r#"<w:bookmarkStart w:id="{id}" w:name="{}"/>"#, escape_attr(&name)));
            }
        }
    };
    let mut i = 0;
    while i < n {
        emit_comment_boundaries(&mut out, i);
        emit_bookmark_boundaries(&mut out, i);
        // A note-reference placeholder is its own run (no text), independent of the
        // surrounding mark set.
        if chars[i] == crate::schema::FIELD_PLACEHOLDER {
            if let Some(note_id) = note_ref_at(marks, i) {
                if let Some(run) = note_reference_run(&note_id, note_ids, note_kinds) {
                    out.push_str(&run);
                }
                i += 1;
                continue;
            }
        }
        let active = marks_at(i, marks);
        let active_change = change_at(marks, i);
        let active_xref = cross_ref_at(marks, i);
        let mut j = i + 1;
        while j < n && !run_breaks_at(&chars, marks, &active, &active_change, &active_xref, j) {
            j += 1;
        }
        // The tracked-change wrapper (if any) for this run group: a `<w:ins>`/`<w:del>`
        // carrying the change's docx id + author/date. Deleted text uses `<w:delText>`.
        let (wrap_open, wrap_close, is_del) = match &active_change {
            Some((kind, id)) => match (changes.get(id), change_ids.get(id)) {
                (Some(ch), Some(docx_id)) => {
                    let close = match kind {
                        crate::snapshot::ChangeKind::Ins => "</w:ins>",
                        crate::snapshot::ChangeKind::Del => "</w:del>",
                    };
                    (change_open_tag(ch, *docx_id), close, *kind == crate::snapshot::ChangeKind::Del)
                }
                _ => (String::new(), "", false),
            },
            None => (String::new(), "", false),
        };
        // A cross-reference wraps the whole run group in a `<w:hyperlink w:anchor>`
        // (the outermost wrapper), so the linked text points at the bookmark/heading.
        let (xref_open, xref_close) = match &active_xref {
            Some(target) => (format!(r#"<w:hyperlink w:anchor="{}">"#, escape_attr(target)), "</w:hyperlink>"),
            None => (String::new(), ""),
        };
        let rpr = run_props(&active);
        out.push_str(&xref_open);
        out.push_str(&wrap_open);
        out.push_str("<w:r>");
        if !rpr.is_empty() {
            out.push_str(&format!("<w:rPr>{rpr}</w:rPr>"));
        }
        if is_del {
            out.push_str(&del_run_content(&chars[i..j]));
        } else {
            out.push_str(&run_content(&chars[i..j]));
        }
        out.push_str("</w:r>");
        out.push_str(wrap_close);
        out.push_str(xref_close);
        i = j;
    }
    // Bookmarks ending exactly at the block's end emit their `w:bookmarkEnd`.
    for name in bookmark_ends_at(marks, n) {
        if let Some(id) = bookmark_ids.get(&name) {
            out.push_str(&format!(r#"<w:bookmarkEnd w:id="{id}"/>"#));
        }
    }
    // Comments ending exactly at the block's end emit their range-end + reference.
    emit_comment_boundaries(&mut out, n);
    out
}

/// Whether char `j` is a note-reference placeholder (so a text run must break
/// before it — the reference is emitted as its own run).
fn is_note_ref_placeholder(chars: &[char], marks: &[crate::document::MarkSpan], j: usize) -> bool {
    chars[j] == crate::schema::FIELD_PLACEHOLDER && note_ref_at(marks, j).is_some()
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

/// The `<w:spacing w:line w:lineRule="auto"/>` for a block's `line_spacing` attr
/// (the multiple → 240ths: 1.5 → 360), or empty when unset/non-positive.
/// Re-imports to the same `line_spacing` via `styles::line_spacing_from_spacing`.
fn spacing_xml(b: &BlockView) -> String {
    match b.attrs.get("line_spacing") {
        Some(AttrVal::Float(ls)) if *ls > 0.0 => {
            let line = (ls * 240.0).round() as i64;
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
        Some(AttrVal::Int(n)) if *n >= 0 => *n as usize,
        _ => 0,
    };
    (g("row"), g("col"))
}

/// Map the model's canonical numbering format onto an OOXML `w:numFmt w:val`
/// (inverse of `styles::map_num_format`).
fn ooxml_num_format(fmt: &str) -> &'static str {
    match fmt {
        "lower-roman" => "lowerRoman",
        "upper-roman" => "upperRoman",
        "lower-alpha" => "lowerLetter",
        "upper-alpha" => "upperLetter",
        _ => "decimal",
    }
}

/// Collects the distinct list numberings a document uses and assigns each a stable
/// `w:numId`, so an ordered list's format + start round-trip. numId 1 is the
/// bullet list and numId 2 the plain decimal-from-1 list (matching the static
/// fallback); non-default ordered numberings get fresh ids from 3 up.
#[derive(Default)]
struct Numbering {
    /// `(ordered, format, start)` → assigned numId.
    ids: Vec<((bool, String, i64), i64)>,
}

impl Numbering {
    /// The `w:numId` for a list item, registering a new id if the numbering is
    /// novel. Bullet and plain decimal-from-1 reuse the fixed ids 1 / 2.
    fn num_id(&mut self, b: &BlockView) -> i64 {
        let ordered = matches!(b.attrs.get("ordered"), Some(AttrVal::Bool(true)));
        if !ordered {
            return 1;
        }
        let fmt = match b.attrs.get("num_format") {
            Some(AttrVal::Str(s)) => s.clone(),
            _ => "decimal".to_string(),
        };
        let start = match b.attrs.get("num_start") {
            Some(AttrVal::Int(n)) => *n,
            _ => 1,
        };
        if fmt == "decimal" && start == 1 {
            return 2;
        }
        let key = (ordered, fmt, start);
        if let Some((_, id)) = self.ids.iter().find(|(k, _)| *k == key) {
            return *id;
        }
        let id = 3 + self.ids.len() as i64;
        self.ids.push((key, id));
        id
    }

    /// Whether any non-default ordered numbering was registered (so the exporter
    /// generates a custom numbering.xml rather than the static fallback).
    fn has_custom(&self) -> bool {
        !self.ids.is_empty()
    }

    /// The full `word/numbering.xml` reconstructing every numbering this document
    /// uses: bullet (1), decimal-from-1 (2), and each registered custom ordered
    /// numbering with its `w:numFmt` + `w:start`.
    fn numbering_xml(&self) -> String {
        let mut abs = String::new();
        let mut nums = String::new();
        // Fixed: abstract 0 = bullet → numId 1; abstract 1 = decimal → numId 2.
        abs.push_str(r#"<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="&#8226;"/></w:lvl></w:abstractNum>"#);
        abs.push_str(r#"<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>"#);
        nums.push_str(r#"<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>"#);
        nums.push_str(r#"<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>"#);
        for ((_, fmt, start), id) in &self.ids {
            let nf = ooxml_num_format(fmt);
            abs.push_str(&format!(
                r#"<w:abstractNum w:abstractNumId="{id}"><w:lvl w:ilvl="0"><w:start w:val="{start}"/><w:numFmt w:val="{nf}"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum>"#
            ));
            nums.push_str(&format!(r#"<w:num w:numId="{id}"><w:abstractNumId w:val="{id}"/></w:num>"#));
        }
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">{abs}{nums}</w:numbering>"#
        )
    }
}

/// EMUs per CSS pixel (914400 EMU/inch ÷ 96 px/inch).
const EMU_PER_PX: f64 = 914400.0 / 96.0;

/// A media part + relationship synthesised for an exported image block.
struct ExportedImage {
    /// Relationship id (e.g. `rIdImg1`) referenced by the drawing's `a:blip`.
    rel_id: String,
    /// Package part name, e.g. `word/media/image1.png`.
    part: String,
    /// `Target` written into the document rels (relative to `word/`).
    target: String,
    /// File extension (`png`/`jpg`/…) for the content-type `Default`.
    ext: &'static str,
    /// Image bytes.
    bytes: Vec<u8>,
}

/// Accumulator threaded through the body build: assigns rel ids + media part names
/// to image blocks and remembers their bytes so `export()` can write the parts +
/// wire the relationships and content-type defaults.
#[derive(Default)]
struct ImageExport {
    images: Vec<ExportedImage>,
}

impl ImageExport {
    /// The distinct `<Default Extension="…" ContentType="…"/>` rows the exported
    /// media parts need in `[Content_Types].xml`.
    fn content_type_defaults(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for img in &self.images {
            if seen.insert(img.ext) {
                let (_, media) = image_ext_and_media(&img.bytes);
                out.push(format!(r#"<Default Extension="{}" ContentType="{}"/>"#, img.ext, media));
            }
        }
        out
    }

    /// The `<Relationship>` rows wiring each media part into the document rels.
    fn relationships(&self) -> String {
        let mut s = String::new();
        for img in &self.images {
            s.push_str(&format!(
                r#"<Relationship Id="{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{}"/>"#,
                img.rel_id, img.target
            ));
        }
        s
    }

    /// Register an image blob, returning its relationship id. The same blob is
    /// stored once per occurrence (Word references each drawing by its own rel).
    fn add(&mut self, bytes: &[u8]) -> String {
        let n = self.images.len() + 1;
        let (ext, _) = image_ext_and_media(bytes);
        let rel_id = format!("rIdImg{n}");
        let target = format!("media/image{n}.{ext}");
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

/// An image block's display size in EMU (`width`/`height` px → EMU), defaulting to
/// a 1-inch square when unset so Word still renders something.
fn image_extent_emu(b: &BlockView) -> (i64, i64) {
    let px = |k: &str, dflt: i64| match b.attrs.get(k) {
        Some(AttrVal::Int(n)) if *n > 0 => *n,
        _ => dflt,
    };
    let cx = (px("width", 96) as f64 * EMU_PER_PX).round() as i64;
    let cy = (px("height", 96) as f64 * EMU_PER_PX).round() as i64;
    (cx, cy)
}

/// Build the `w:drawing` XML for an image block, registering its blob with `imgs`.
/// Returns `None` if the blob is absent. Inline images emit `wp:inline`; floating
/// images emit `wp:anchor` with the appropriate `wp:wrap*` element + a position.
fn image_drawing_xml(b: &BlockView, snap: &DocSnapshot, imgs: &mut ImageExport) -> Option<String> {
    let hash = match b.attrs.get("blob") {
        Some(AttrVal::Str(h)) => h,
        _ => return None,
    };
    let bytes = snap.blobs.get(hash)?;
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
        let rel_from_v = if anchor == "page" { "page" } else { "paragraph" };
        let wrap = match image_wrap(b) {
            "left" => r#"<wp:wrapSquare wrapText="left"/>"#.to_string(),
            "right" => r#"<wp:wrapSquare wrapText="right"/>"#.to_string(),
            "both" => r#"<wp:wrapSquare wrapText="bothSides"/>"#.to_string(),
            _ => r#"<wp:wrapNone/>"#.to_string(),
        };
        Some(format!(
            r#"<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="0" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="{rel_from}"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="{rel_from_v}"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>{wrap}{graphic}</wp:anchor></w:drawing></w:r>"#
        ))
    }
}

/// Build `word/document.xml` and collect the set of CRDT styles-container styleIds
/// referenced by a `<w:pStyle>` (so the caller can emit their definitions), plus
/// the list-numbering plan (so the caller can write a matching numbering.xml).
/// `note_ids`/`note_kinds` map each qdoc note id to its emitted docx id + kind, so a
/// `note-ref` placeholder renders the right `w:footnoteReference`/`w:endnoteReference`.
fn build_document_xml(
    snap: &DocSnapshot,
    sectpr: &str,
    note_ids: &BTreeMap<String, i64>,
    note_kinds: &BTreeMap<String, crate::model::NoteKind>,
    comment_ids: &BTreeMap<String, i64>,
) -> (String, BTreeSet<String>, Numbering, ImageExport) {
    let mut body = String::new();
    let mut named_refs: BTreeSet<String> = BTreeSet::new();
    let mut numbering = Numbering::default();
    let mut imgs = ImageExport::default();
    // Tracked changes: a `<w:ins>`/`<w:del>` wrapper carries an integer revision id.
    // Assigned from 0 in canonical (sorted) order — independent of comment ids.
    let changes = change_index(snap);
    let change_ids: BTreeMap<String, i64> = snap.changes.iter().enumerate().map(|(i, c)| (c.id.clone(), i as i64)).collect();
    let changes = &changes;
    let change_ids = &change_ids;
    // Bookmarks: `w:bookmarkStart`/`w:bookmarkEnd` carry an integer `w:id` per name.
    // Assigned from 0 in sorted-name order so output is deterministic.
    let bookmark_ids: BTreeMap<String, i64> = {
        let mut names: Vec<String> = snap.bookmarks.iter().map(|bm| bm.name.clone()).collect();
        names.sort();
        names.dedup();
        names.into_iter().enumerate().map(|(i, name)| (name, i as i64)).collect()
    };
    let bookmark_ids = &bookmark_ids;
    let mut i = 0;
    while i < snap.blocks.len() {
        let b = &snap.blocks[i];
        match b.kind.as_str() {
            "heading" => {
                let level = match b.attrs.get("level") {
                    Some(AttrVal::Int(n)) => (*n).clamp(1, 6),
                    _ => 1,
                };
                body.push_str(&format!(
                    r#"<w:p><w:pPr><w:pStyle w:val="Heading{level}"/>{}{}</w:pPr>{}</w:p>"#,
                    jc_xml(b),
                    spacing_xml(b),
                    render_runs(&b.text, &b.marks, note_ids, note_kinds, comment_ids, changes, change_ids, bookmark_ids)
                ));
                i += 1;
            }
            "list_item" => {
                let numid = numbering.num_id(b);
                body.push_str(&format!(
                    r#"<w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="{numid}"/></w:numPr>{}{}</w:pPr>{}</w:p>"#,
                    jc_xml(b),
                    spacing_xml(b),
                    render_runs(&b.text, &b.marks, note_ids, note_kinds, comment_ids, changes, change_ids, bookmark_ids)
                ));
                i += 1;
            }
            "image" => {
                // Wrap the drawing run in a paragraph (Word images live in a `w:p`).
                if let Some(drawing) = image_drawing_xml(b, snap, &mut imgs) {
                    body.push_str(&format!("<w:p>{drawing}</w:p>"));
                }
                i += 1;
            }
            "fidelity" => {
                if let Some(AttrVal::Str(xml)) = b.attrs.get("xml") {
                    body.push_str(xml);
                }
                i += 1;
            }
            "cell" => {
                // Emit a run of same-`table` cells as a real OOXML table.
                let tid = cell_table(b).unwrap_or_default();
                let start = i;
                while i < snap.blocks.len() && cell_table(&snap.blocks[i]).as_deref() == Some(tid.as_str()) {
                    i += 1;
                }
                let cells = &snap.blocks[start..i];
                let nrows = cells.iter().map(|c| cell_rc(c).0).max().unwrap_or(0) + 1;
                let ncols = cells.iter().map(|c| cell_rc(c).1).max().unwrap_or(0) + 1;
                body.push_str(r#"<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid>"#);
                for _ in 0..ncols {
                    body.push_str("<w:gridCol/>");
                }
                body.push_str("</w:tblGrid>");
                for r in 0..nrows {
                    body.push_str("<w:tr>");
                    for c in 0..ncols {
                        let inner = cells
                            .iter()
                            .find(|cl| cell_rc(cl) == (r, c))
                            .map(|cl| render_runs(&cl.text, &cl.marks, note_ids, note_kinds, comment_ids, changes, change_ids, bookmark_ids))
                            .unwrap_or_default();
                        body.push_str(&format!(
                            r#"<w:tc><w:tcPr><w:tcW w:w="0" w:type="auto"/></w:tcPr><w:p>{inner}</w:p></w:tc>"#
                        ));
                    }
                    body.push_str("</w:tr>");
                }
                body.push_str("</w:tbl>");
            }
            _ => {
                // A plain paragraph. A `style` attr referencing a container style
                // emits a `<w:pStyle>` (and registers the styleId for definition);
                // a direct `align` attr still rides as a `<w:jc>`.
                let mut ppr_inner = String::new();
                if let Some(AttrVal::Str(sid)) = b.attrs.get("style") {
                    if snap.styles.contains_key(sid) {
                        ppr_inner.push_str(&format!(r#"<w:pStyle w:val="{}"/>"#, escape_attr(sid)));
                        named_refs.insert(sid.clone());
                    }
                }
                ppr_inner.push_str(&jc_xml(b));
                ppr_inner.push_str(&spacing_xml(b));
                let ppr = if ppr_inner.is_empty() { String::new() } else { format!("<w:pPr>{ppr_inner}</w:pPr>") };
                body.push_str(&format!("<w:p>{ppr}{}</w:p>", render_runs(&b.text, &b.marks, note_ids, note_kinds, comment_ids, changes, change_ids, bookmark_ids)));
                i += 1;
            }
        }
    }

    // The section properties (with header/footer references) close the body. A
    // `w:sectPr` is a direct child of `w:body`, after the last block.
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml"><w:body>{body}{sectpr}</w:body></w:document>"#
    );
    (document, named_refs, numbering, imgs)
}

#[cfg(test)]
mod tests {
    use super::super::container::{minimal_docx, minimal_docx_styled, DocxContainer};
    use super::super::DocxCodec;
    use crate::document::MarkValue;
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
    fn norm_region(blocks: &[crate::snapshot::BlockView]) -> Vec<(String, Vec<(usize, usize, String, MarkValue)>)> {
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
        let body = concat!(
            r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:t xml:space="preserve">a </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:t xml:space="preserve"> b</w:t></w:r></w:p>"#,
        );
        let docx = minimal_docx_styled("", body);
        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let m1 = norm(&d.materialize());

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let c = DocxContainer::read(&exported).unwrap();
        assert!(c.get("word/document.xml").is_some());
        assert!(c.get("[Content_Types].xml").is_some());
        assert!(c.get("_rels/.rels").is_some());

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let m2 = norm(&d2.materialize());
        assert_eq!(m1, m2, "model round-trips through .docx export then import");
    }

    // A 1×1 PNG, used as image blob bytes in the image round-trip test.
    const TINY_PNG: [u8; 69] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
        0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
        0x00, 0x03, 0x01, 0x01, 0x00, 0xf7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
        0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn image_inline_roundtrips_through_docx() {
        // A plain inline image with an explicit display size must round-trip with
        // its blob bytes, `anchor=inline`, and size intact — plus emit a real
        // `wp:inline` drawing, a media part, a relationship and a content-type.
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
            dm.set_attr(&img, "width", 64_i64);
            dm.set_attr(&img, "height", 48_i64);
        }

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        // Media part + relationship + content-type default written.
        let media = cont
            .parts
            .iter()
            .find(|(n, _)| n.starts_with("word/media/") && n.ends_with(".png"))
            .expect("media part written");
        assert_eq!(media.1, &TINY_PNG[..], "image bytes written verbatim");
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains("<wp:inline"), "inline drawing emitted: {document}");
        let rels = std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="media/image1.png""#), "image rel wired: {rels}");
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(ct.contains(r#"Extension="png""#), "png content-type default: {ct}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let im = snap.blocks.iter().find(|b| b.kind == "image").expect("image block present");
        let blob_hash = match im.attrs.get("blob") {
            Some(AttrVal::Str(h)) => h.clone(),
            other => panic!("image missing blob attr: {other:?}"),
        };
        assert_eq!(blob_hash, hash, "blake3 hash stable across round-trip");
        assert_eq!(snap.blobs.get(&blob_hash).map(|b| b.as_slice()), Some(&TINY_PNG[..]), "blob bytes survive");
        assert_eq!(im.attrs.get("anchor"), Some(&AttrVal::Str("inline".into())), "{:?}", im.attrs);
        assert_eq!(im.attrs.get("width"), Some(&AttrVal::Int(64)), "{:?}", im.attrs);
        assert_eq!(im.attrs.get("height"), Some(&AttrVal::Int(48)), "{:?}", im.attrs);
    }

    #[test]
    fn image_positioning_roundtrips_through_docx() {
        // A floating, paragraph-anchored image that wraps text on its right, with an
        // explicit display size, must survive a .docx round-trip with anchor + wrap +
        // size intact (emitting a `wp:anchor` + `wp:wrapSquare wrapText="right"`).
        let mut d = Document::new();
        let p1 = d.insert_block(None, "paragraph");
        d.splice_text(&p1, 0, 0, "text");
        let img = d.insert_block(Some(&p1), "image");
        let hash = d.put_blob(&TINY_PNG);
        {
            let mut dm = d.as_mut();
            dm.set_attr(&img, "blob", hash.as_str());
            dm.set_attr(&img, "anchor", "paragraph");
            dm.set_attr(&img, "wrap", "right");
            dm.set_attr(&img, "width", 120_i64);
            dm.set_attr(&img, "height", 80_i64);
        }

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains("<wp:anchor"), "anchor drawing emitted: {document}");
        assert!(document.contains(r#"<wp:wrapSquare wrapText="right"/>"#), "right wrap emitted: {document}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let im = snap.blocks.iter().find(|b| b.kind == "image").expect("image block present");
        assert_eq!(im.attrs.get("anchor"), Some(&AttrVal::Str("paragraph".into())), "{:?}", im.attrs);
        assert_eq!(im.attrs.get("wrap"), Some(&AttrVal::Str("right".into())), "{:?}", im.attrs);
        assert_eq!(im.attrs.get("width"), Some(&AttrVal::Int(120)), "{:?}", im.attrs);
        assert_eq!(im.attrs.get("height"), Some(&AttrVal::Int(80)), "{:?}", im.attrs);
    }

    #[test]
    fn heading_paragraph_props_roundtrip() {
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

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].attrs.get("level"), Some(&AttrVal::Int(3)));
        let b = &snap.blocks[1];
        assert_eq!(b.text, "styled");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("center".into())));
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(22)), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "{:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "underline"), "{:?}", b.marks);
    }

    #[test]
    fn highlight_super_and_line_spacing_roundtrip() {
        // A run with `highlight` + a `super` run + a paragraph-level `line_spacing`
        // attr must all survive the .docx export → import cycle.
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "E=mc2");
        {
            let mut dm = d.as_mut();
            dm.add_mark(&id, 0, 5, "highlight", "#ffff00");
            dm.add_mark(&id, 4, 5, "super", true);
            dm.set_attr(&id, "line_spacing", 1.5_f64);
        }

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        // The export carries the OOXML properties we expect.
        let c = DocxContainer::read(&exported).unwrap();
        let xml = std::str::from_utf8(c.get("word/document.xml").unwrap()).unwrap();
        assert!(xml.contains(r#"w:shd w:val="clear" w:color="auto" w:fill="FFFF00""#), "highlight shd: {xml}");
        assert!(xml.contains(r#"<w:vertAlign w:val="superscript"/>"#), "super vertAlign: {xml}");
        assert!(xml.contains(r#"<w:spacing w:line="360" w:lineRule="auto"/>"#), "line spacing: {xml}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let b = &snap.blocks[0];
        assert_eq!(b.text, "E=mc2");
        assert!(
            b.marks.iter().any(|m| m.name == "highlight" && m.value == MarkValue::Str("#ffff00".into()) && m.start == 0 && m.end == 5),
            "highlight mark: {:?}",
            b.marks
        );
        assert!(
            b.marks.iter().any(|m| m.name == "super" && m.value == MarkValue::Bool(true) && m.start == 4 && m.end == 5),
            "super mark: {:?}",
            b.marks
        );
        assert_eq!(b.attrs.get("line_spacing"), Some(&AttrVal::Float(1.5)), "line_spacing attr: {:?}", b.attrs);
    }

    #[test]
    fn subscript_roundtrips_through_docx() {
        // A `sub` run exports as `<w:vertAlign w:val="subscript"/>` and re-imports as `sub`.
        let mut d = Document::new();
        let id = d.insert_block(None, "paragraph");
        d.splice_text(&id, 0, 0, "H2O");
        d.as_mut().add_mark(&id, 1, 2, "sub", true);

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let b = &snap.blocks[0];
        assert!(
            b.marks.iter().any(|m| m.name == "sub" && m.value == MarkValue::Bool(true) && m.start == 1 && m.end == 2),
            "sub mark: {:?}",
            b.marks
        );
    }

    #[test]
    fn bookmark_and_cross_reference_roundtrip_through_docx() {
        // A bookmark "intro" over a word + a cross-reference pointing at it must survive
        // a `.docx` export+import with both names and ranges intact.
        let mut d = Document::new();
        let target = d.insert_block(None, "paragraph");
        d.splice_text(&target, 0, 0, "the intro section");
        d.add_bookmark(&target, 4, 9, "intro"); // over "intro"
        let src = d.insert_block(Some(&target), "paragraph");
        d.splice_text(&src, 0, 0, "see above");
        d.add_cross_ref(&src, 4, 9, "intro"); // "above" → #intro

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        // Bookmark markers + the cross-ref hyperlink anchor appear in the markup.
        assert!(document.contains(r#"<w:bookmarkStart w:id="0" w:name="intro"/>"#), "bookmarkStart emitted: {document}");
        assert!(document.contains(r#"<w:bookmarkEnd w:id="0"/>"#), "bookmarkEnd emitted: {document}");
        assert!(document.contains(r#"<w:hyperlink w:anchor="intro">"#), "cross-ref hyperlink emitted: {document}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        // The bookmark survives with its name + range.
        assert_eq!(snap.bookmarks.len(), 1, "one bookmark survives: {:?}", snap.bookmarks);
        let bm = &snap.bookmarks[0];
        assert_eq!(bm.name, "intro");
        assert_eq!((bm.range.1, bm.range.2), (4, 9), "bookmark range over 'intro': {:?}", bm);
        // The bookmarked block carries the `bookmark` mark.
        let tb = snap.blocks.iter().find(|b| b.text == "the intro section").unwrap();
        let mark = tb.marks.iter().find(|m| m.name == crate::schema::MARK_BOOKMARK).expect("bookmark mark present");
        assert_eq!((mark.start, mark.end), (4, 9));
        assert_eq!(mark.value, MarkValue::Str("intro".into()));
        // The cross-reference survives as a `link` mark valued `#intro`.
        let sb = snap.blocks.iter().find(|b| b.text == "see above").unwrap();
        let link = sb.marks.iter().find(|m| m.name == "link").expect("cross-ref link mark present");
        assert_eq!((link.start, link.end), (4, 9), "cross-ref over 'above': {:?}", sb.marks);
        assert_eq!(link.value, MarkValue::Str("#intro".into()));
    }

    #[test]
    fn lists_roundtrip_with_ordering() {
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

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|x| x.kind == "list_item").collect();
        assert_eq!(items.len(), 2, "{snap:?}");
        assert_eq!(items[0].text, "first");
        assert_eq!(items[0].attrs.get("ordered"), Some(&AttrVal::Bool(true)));
        assert_eq!(items[1].attrs.get("ordered"), Some(&AttrVal::Bool(false)));
    }

    #[test]
    fn list_numbering_format_and_start_roundtrip_through_docx() {
        // An upper-roman ordered list starting at 3: format + start must survive a
        // .docx export then re-import (fidelity preservation).
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

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let numbering = std::str::from_utf8(cont.get("word/numbering.xml").unwrap()).unwrap();
        assert!(numbering.contains(r#"<w:numFmt w:val="upperRoman"/>"#), "upper-roman numFmt emitted: {numbering}");
        assert!(numbering.contains(r#"<w:start w:val="3"/>"#), "start value emitted: {numbering}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let items: Vec<_> = snap.blocks.iter().filter(|x| x.kind == "list_item").collect();
        assert_eq!(items.len(), 2, "{snap:?}");
        for it in &items {
            assert_eq!(it.attrs.get("ordered"), Some(&AttrVal::Bool(true)), "still ordered: {:?}", it.attrs);
            assert_eq!(it.attrs.get("num_format"), Some(&AttrVal::Str("upper-roman".into())), "format survives: {:?}", it.attrs);
            assert_eq!(it.attrs.get("num_start"), Some(&AttrVal::Int(3)), "start survives: {:?}", it.attrs);
        }

        // Idempotent: a second round-trip preserves the same numbering attrs.
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(norm(&d2.materialize()), norm(&d3.materialize()), "list-numbering model stable across a second round-trip");
    }

    #[test]
    fn colored_serif_20pt_justified_run_roundtrips_through_docx() {
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
        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains(r#"<w:jc w:val="both"/>"#), "justify → w:jc both: {document}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let b = &d2.materialize().blocks[0];
        assert_eq!(b.text, "styled");
        assert_eq!(b.attrs.get("align"), Some(&AttrVal::Str("justify".into())), "justify survives: {:?}", b.attrs);
        assert!(b.marks.iter().any(|m| m.name == "color" && m.value == MarkValue::Str("#3366cc".into())), "colour: {:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontFamily" && m.value == MarkValue::Str("serif".into())), "serif: {:?}", b.marks);
        assert!(b.marks.iter().any(|m| m.name == "fontSize" && m.value == MarkValue::Int(20)), "20pt: {:?}", b.marks);
    }

    #[test]
    fn table_roundtrips_as_editable_cells() {
        let mut d = Document::new();
        let mut prev: Option<String> = None;
        for (r, c, t) in [(0, 0, "a"), (0, 1, "b"), (1, 0, "c"), (1, 1, "d")] {
            let id = d.insert_block(prev.as_deref(), "cell");
            d.splice_text(&id, 0, 0, t);
            {
                let mut dm = d.as_mut();
                dm.set_attr(&id, "table", "T1");
                dm.set_attr(&id, "row", r as i64);
                dm.set_attr(&id, "col", c as i64);
            }
            prev = Some(id);
        }
        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        let cells: Vec<_> = snap.blocks.iter().filter(|b| b.kind == "cell").collect();
        assert_eq!(cells.iter().map(|c| c.text.clone()).collect::<Vec<_>>(), vec!["a", "b", "c", "d"], "{snap:?}");
        assert_eq!(cells[3].attrs.get("row"), Some(&AttrVal::Int(1)));
        assert_eq!(cells[3].attrs.get("col"), Some(&AttrVal::Int(1)));
        // Cells stay editable: their text lives in a real Text object.
        let id = &cells[0].id;
        d2.splice_text(id, 1, 0, "X");
        assert_eq!(d2.block_text(id).as_deref(), Some("aX"));
    }

    #[test]
    fn named_paragraph_style_roundtrips_through_docx() {
        // A paragraph that references a custom CRDT-native named style must export
        // as a real OOXML `w:style w:type="paragraph"` + `w:pStyle`, and re-import
        // with the style intact and still applied to the block. Mirrors the `.odt`
        // named-style round-trip.
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

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains(r#"<w:pStyle w:val="Fancy"/>"#), "paragraph references the named style: {document}");
        let styles = std::str::from_utf8(cont.get("word/styles.xml").unwrap()).unwrap();
        assert!(
            styles.contains(r#"<w:style w:type="paragraph" w:styleId="Fancy">"#),
            "named style defined as OOXML w:style: {styles}"
        );
        assert!(
            styles.contains("<w:b/>") && styles.contains("<w:i/>") && styles.contains(r#"<w:sz w:val="40"/>"#),
            "props emitted: {styles}"
        );
        assert!(styles.contains(r#"<w:jc w:val="center"/>"#) && styles.contains(r#"<w:color w:val="FF0000"/>"#), "pPr/colour emitted: {styles}");
        // The freshly-synthesised styles part is wired into content-types + rels.
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(ct.contains("/word/styles.xml"), "styles override registered: {ct}");
        let rels = std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="styles.xml""#), "styles relationship registered: {rels}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
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
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(norm(&d2.materialize()), norm(&d3.materialize()), "named-style model is stable across a second round-trip");
    }

    #[test]
    fn document_properties_roundtrip_through_docx() {
        // Title/author/subject/keywords/dates/generator on a fresh doc must export
        // into docProps/core.xml (+ app.xml generator) and re-import unchanged.
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

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let core = std::str::from_utf8(cont.get("docProps/core.xml").expect("core.xml written")).unwrap();
        assert!(core.contains("<dc:title>My Title</dc:title>"), "title in core.xml: {core}");
        assert!(core.contains("<dc:creator>Ada Lovelace</dc:creator>"), "creator: {core}");
        assert!(core.contains("<dc:subject>Analytical Engine</dc:subject>"), "subject: {core}");
        assert!(core.contains("<cp:keywords>computing, history</cp:keywords>"), "keywords: {core}");
        assert!(core.contains("2020-01-02T03:04:05Z</dcterms:created>"), "created: {core}");
        assert!(core.contains("2021-06-07T08:09:10Z</dcterms:modified>"), "modified: {core}");
        let app = std::str::from_utf8(cont.get("docProps/app.xml").expect("app.xml written")).unwrap();
        assert!(app.contains("<Application>qdoc/1.0</Application>"), "generator in app.xml: {app}");
        // Freshly-synthesised parts are wired into content-types + the root rels.
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(ct.contains("/docProps/core.xml") && ct.contains("/docProps/app.xml"), "overrides registered: {ct}");
        let rels = std::str::from_utf8(cont.get("_rels/.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="docProps/core.xml""#) && rels.contains(r#"Target="docProps/app.xml""#), "rels registered: {rels}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let props = d2.doc_props();
        assert_eq!(props.get("title").map(String::as_str), Some("My Title"));
        assert_eq!(props.get("author").map(String::as_str), Some("Ada Lovelace"));
        assert_eq!(props.get("subject").map(String::as_str), Some("Analytical Engine"));
        assert_eq!(props.get("keywords").map(String::as_str), Some("computing, history"));
        assert_eq!(props.get("created").map(String::as_str), Some("2020-01-02T03:04:05Z"));
        assert_eq!(props.get("modified").map(String::as_str), Some("2021-06-07T08:09:10Z"));
        assert_eq!(props.get("generator").map(String::as_str), Some("qdoc/1.0"));
        assert_eq!(d2.materialize().blocks[0].text, "body");

        // Idempotent across a second round-trip.
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(d2.doc_props(), d3.doc_props(), "properties stable across a second round-trip");
    }

    #[test]
    fn core_xml_preserves_unmodeled_fields_and_updates_modeled() {
        // An imported core.xml with an unmodeled field (revision) plus a title:
        // the unmodeled field survives and the title updates on export.
        let core = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/"><dc:title>Old</dc:title><cp:revision>4</cp:revision></cp:coreProperties>"#;
        let base = minimal_docx_styled("", "<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
        let mut c = DocxContainer::read(&base).unwrap();
        c.set("docProps/core.xml", core.as_bytes().to_vec());
        let docx = c.write().unwrap();

        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        assert_eq!(d.doc_prop("title").as_deref(), Some("Old"), "title imported from core.xml");
        d.set_doc_prop("title", "New");

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let out = DocxContainer::read(&exported).unwrap();
        let core_out = std::str::from_utf8(out.get("docProps/core.xml").unwrap()).unwrap();
        assert!(core_out.contains("<dc:title>New</dc:title>"), "title updated: {core_out}");
        assert!(!core_out.contains("<dc:title>Old</dc:title>"), "old title gone: {core_out}");
        assert!(core_out.contains("revision>4</"), "unmodeled field preserved: {core_out}");
    }

    #[test]
    fn no_docprops_when_no_properties_and_none_imported() {
        // A from-scratch doc with no properties must not synthesise docProps parts.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "plain");
        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        assert!(cont.get("docProps/core.xml").is_none(), "no core.xml for a doc with no properties");
        assert!(cont.get("docProps/app.xml").is_none(), "no app.xml for a doc with no properties");
    }

    #[test]
    fn header_footer_with_page_number_roundtrip_through_docx() {
        use crate::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_FIELD};
        // A header "Title" + footer with a page-number field must export as
        // header1.xml/footer1.xml wired through sectPr + content-types + rels, and
        // re-import as the same header/footer + field.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "body");
        let h = d.insert_header_block(None, "paragraph");
        d.splice_header_text(&h, 0, 0, "Title");
        let f = d.insert_footer_block(None, "paragraph");
        d.splice_footer_text(&f, 0, 0, "Page ");
        d.insert_footer_page_number(&f, 5);

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        // The header/footer parts exist and carry the content + a PAGE field.
        let hdr = std::str::from_utf8(cont.get("word/header1.xml").expect("header1.xml written")).unwrap();
        assert!(hdr.contains("<w:hdr") && hdr.contains("Title"), "header part holds the title: {hdr}");
        let ftr = std::str::from_utf8(cont.get("word/footer1.xml").expect("footer1.xml written")).unwrap();
        assert!(ftr.contains("<w:ftr"), "footer part is a w:ftr: {ftr}");
        assert!(ftr.contains(r#"w:instr=" PAGE ""#), "PAGE field emitted: {ftr}");
        // The body's sectPr references them.
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains("<w:sectPr>"), "sectPr present: {document}");
        assert!(document.contains(r#"<w:headerReference w:type="default" r:id="rIdQdocHdr"/>"#), "header ref: {document}");
        assert!(document.contains(r#"<w:footerReference w:type="default" r:id="rIdQdocFtr"/>"#), "footer ref: {document}");
        // Content-types + rels wired.
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(ct.contains("/word/header1.xml") && ct.contains("/word/footer1.xml"), "overrides registered: {ct}");
        let rels = std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="header1.xml""#) && rels.contains(r#"Target="footer1.xml""#), "rels registered: {rels}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        assert_eq!(d2.materialize().blocks[0].text, "body", "body intact");
        let header = d2.header();
        assert_eq!(header.len(), 1, "header block present: {header:?}");
        assert_eq!(header[0].text, "Title");
        let footer = d2.footer();
        assert_eq!(footer.len(), 1, "footer block present: {footer:?}");
        assert_eq!(footer[0].text, format!("Page {}", FIELD_PLACEHOLDER), "placeholder kept: {:?}", footer[0].text);
        let field = footer[0].marks.iter().find(|m| m.name == MARK_FIELD).expect("page-number field survives");
        assert_eq!((field.start, field.end), (5, 6), "field spans the placeholder: {:?}", footer[0].marks);
        assert_eq!(field.value, MarkValue::Str(FIELD_PAGE_NUMBER.into()));

        // Idempotent across a second round-trip (block ids are volatile per import,
        // so compare on text + marks, not the full BlockView).
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        assert_eq!(norm_region(&d2.header()), norm_region(&d3.header()), "header stable across a second round-trip");
        assert_eq!(norm_region(&d2.footer()), norm_region(&d3.footer()), "footer stable across a second round-trip");
    }

    #[test]
    fn imports_complex_page_field_from_header() {
        // A header using the complex-field form (fldChar begin/instrText PAGE/
        // separate/end) must import as a single page-number field, dropping the
        // cached result text.
        use crate::schema::{FIELD_PLACEHOLDER, MARK_FIELD};
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
        // Point rId7 at the header part.
        let rels = std::str::from_utf8(c.get("word/_rels/document.xml.rels").unwrap()).unwrap().to_string();
        let rels = rels.replace(
            "</Relationships>",
            r#"<Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/></Relationships>"#,
        );
        c.set("word/_rels/document.xml.rels", rels.into_bytes());
        let docx = c.write().unwrap();

        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let header = d.header();
        assert_eq!(header.len(), 1, "{header:?}");
        // "p. " + a single placeholder; the cached "7" result is dropped.
        assert_eq!(header[0].text, format!("p. {}", FIELD_PLACEHOLDER), "complex field → one placeholder: {:?}", header[0].text);
        assert!(header[0].marks.iter().any(|m| m.name == MARK_FIELD && m.start == 3 && m.end == 4), "field mark: {:?}", header[0].marks);
    }

    #[test]
    fn footnote_imports_from_footnotes_part() {
        use crate::document::MarkValue;
        use crate::schema::{FIELD_PLACEHOLDER, MARK_NOTE_REF};
        // A body paragraph "see ref" referencing footnote id 2, whose content lives
        // in word/footnotes.xml. Import must place a note-ref marker + create the note.
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
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let p = &snap.blocks[0];
        assert_eq!(p.text, format!("see {}ref", FIELD_PLACEHOLDER), "placeholder spliced inline: {:?}", p.text);
        let nref = p.marks.iter().find(|m| m.name == MARK_NOTE_REF).expect("note-ref mark present");
        assert_eq!((nref.start, nref.end), (4, 5), "marker covers the placeholder: {:?}", p.marks);
        let nid = match &nref.value {
            MarkValue::Str(s) => s.clone(),
            other => panic!("note-ref value is the note id: {other:?}"),
        };
        assert_eq!(snap.notes.len(), 1, "one note, separators skipped: {:?}", snap.notes);
        assert_eq!(snap.notes[0].id, nid);
        assert_eq!(snap.notes[0].kind, "footnote");
        assert_eq!(snap.notes[0].blocks[0].text, "the footnote");
    }

    #[test]
    fn footnote_and_endnote_roundtrip_through_docx() {
        use crate::document::MarkValue;
        use crate::schema::MARK_NOTE_REF;
        // Build from scratch: a footnote after "see " in one paragraph, an endnote in
        // a second. Export must write footnotes.xml/endnotes.xml + the references +
        // content-types/rels, and re-import as the same markers + notes + kinds.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "see ref");
        let fid = d.insert_footnote(&p, 4);
        let fb = d.insert_note_block(&fid, None, "paragraph").unwrap();
        d.splice_note_text(&fid, &fb, 0, 0, "a footnote");

        let p2 = d.insert_block(Some(&p), "paragraph");
        d.splice_text(&p2, 0, 0, "tail");
        let eid = d.insert_endnote(&p2, 4);
        let eb = d.insert_note_block(&eid, None, "paragraph").unwrap();
        d.splice_note_text(&eid, &eb, 0, 0, "an endnote");

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        // Parts written.
        let ftn = std::str::from_utf8(cont.get("word/footnotes.xml").expect("footnotes.xml written")).unwrap();
        assert!(ftn.contains("a footnote"), "footnote body emitted: {ftn}");
        let end = std::str::from_utf8(cont.get("word/endnotes.xml").expect("endnotes.xml written")).unwrap();
        assert!(end.contains("an endnote"), "endnote body emitted: {end}");
        // Body references emitted.
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains("<w:footnoteReference"), "footnote reference emitted: {document}");
        assert!(document.contains("<w:endnoteReference"), "endnote reference emitted: {document}");
        // Wired into content-types + rels.
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(ct.contains("/word/footnotes.xml") && ct.contains("/word/endnotes.xml"), "note parts in content-types: {ct}");
        let rels = std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="footnotes.xml""#) && rels.contains(r#"Target="endnotes.xml""#), "note rels registered: {rels}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        assert_eq!(snap.blocks[0].text.chars().filter(|c| *c != '\u{FFFC}').collect::<String>(), "see ref");
        let nref = snap.blocks[0].marks.iter().find(|m| m.name == MARK_NOTE_REF).expect("footnote marker survives");
        let f_id = match &nref.value {
            MarkValue::Str(s) => s.clone(),
            _ => panic!("note-ref value"),
        };
        assert_eq!(snap.notes.len(), 2, "both notes survive: {:?}", snap.notes);
        let foot = snap.notes.iter().find(|n| n.id == f_id).expect("footnote present by id");
        assert_eq!(foot.kind, "footnote");
        assert_eq!(foot.blocks[0].text, "a footnote");
        let end_note = snap.notes.iter().find(|n| n.kind == "endnote").expect("endnote present");
        assert_eq!(end_note.blocks[0].text, "an endnote");

        // Idempotent across a second round-trip (ids volatile; compare kind+text).
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        let kinds_texts = |snap: &DocSnapshot| {
            let mut v: Vec<(String, String)> =
                snap.notes.iter().map(|n| (n.kind.clone(), n.blocks.iter().map(|b| b.text.clone()).collect())).collect();
            v.sort();
            v
        };
        assert_eq!(kinds_texts(&d2.materialize()), kinds_texts(&d3.materialize()), "notes stable across a second round-trip");
    }

    #[test]
    fn comment_imports_from_comments_part() {
        use crate::document::MarkValue;
        use crate::schema::MARK_COMMENT;
        // A body paragraph "fix this word" whose "this word" is wrapped in a comment
        // range id 0, with content in word/comments.xml. Import must place a `comment`
        // mark over the range + create the comment with author/date/body.
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
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let snap = d.materialize();
        let p = &snap.blocks[0];
        assert_eq!(p.text, "fix this word", "comment markup not in text: {:?}", p.text);
        let cm = p.marks.iter().find(|m| m.name == MARK_COMMENT).expect("comment mark present");
        assert_eq!((cm.start, cm.end), (4, 13), "marker spans 'this word': {:?}", p.marks);
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

    #[test]
    fn comment_roundtrips_through_docx() {
        use crate::document::MarkValue;
        use crate::schema::MARK_COMMENT;
        // Build from scratch: a comment over "this word". Export must write
        // comments.xml + the range markers + reference + content-types/rels, and
        // re-import as the same anchored range + comment author/text.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "please fix this word now");
        d.add_comment(&p, 11, 20, "Ada Lovelace", "note: fix"); // over "this word"

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        // The comments part is written with author + body.
        let cmts = std::str::from_utf8(cont.get("word/comments.xml").expect("comments.xml written")).unwrap();
        assert!(cmts.contains(r#"w:author="Ada Lovelace""#), "author emitted: {cmts}");
        assert!(cmts.contains("note: fix"), "comment body emitted: {cmts}");
        // Body range markers + reference emitted.
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains("<w:commentRangeStart"), "range start emitted: {document}");
        assert!(document.contains("<w:commentRangeEnd"), "range end emitted: {document}");
        assert!(document.contains("<w:commentReference"), "reference emitted: {document}");
        // Wired into content-types + rels.
        let ct = std::str::from_utf8(cont.get("[Content_Types].xml").unwrap()).unwrap();
        assert!(ct.contains("/word/comments.xml"), "comments part in content-types: {ct}");
        let rels = std::str::from_utf8(cont.get("word/_rels/document.xml.rels").unwrap()).unwrap();
        assert!(rels.contains(r#"Target="comments.xml""#), "comments rel registered: {rels}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        assert_eq!(snap.blocks[0].text, "please fix this word now");
        let cm = snap.blocks[0].marks.iter().find(|m| m.name == MARK_COMMENT).expect("comment marker survives");
        assert_eq!((cm.start, cm.end), (11, 20), "anchored range survives: {:?}", snap.blocks[0].marks);
        let new_id = match &cm.value {
            MarkValue::Str(s) => s.clone(),
            _ => panic!("comment value"),
        };
        let cmt = snap.comments.iter().find(|c| c.id == new_id).expect("comment present by id");
        assert_eq!(cmt.author, "Ada Lovelace");
        assert_eq!(cmt.blocks[0].text, "note: fix");
        assert_eq!(cmt.range, Some((snap.blocks[0].id.clone(), 11, 20)));

        // Idempotent across a second round-trip (ids volatile; compare author+text+span).
        let exported2 = DocxCodec.export(d2.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
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
    fn tracked_changes_roundtrip_through_docx() {
        use crate::document::MarkValue;
        use crate::schema::{MARK_DEL, MARK_INS};
        // A tracked insertion + a tracked deletion (with author) must export as
        // <w:ins>/<w:del> run wrappers (deleted text via <w:delText>), and re-import
        // with kind/author + anchored ranges intact.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        let ins = d.track_insert(&p, 5, " there", "Ada", "2021-01-02T00:00:00Z");
        let del = d.track_delete(&p, 12, 17, "Bob", "2021-03-04T00:00:00Z"); // "world"
        let _ = (ins, del);

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let cont = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(cont.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains("<w:ins "), "ins wrapper emitted: {document}");
        assert!(document.contains("<w:del "), "del wrapper emitted: {document}");
        assert!(document.contains("<w:delText"), "deleted text uses delText: {document}");
        assert!(document.contains(r#"w:author="Ada""#), "ins author emitted: {document}");
        assert!(document.contains(r#"w:author="Bob""#), "del author emitted: {document}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let snap = d2.materialize();
        // The deleted text is retained (struck), the inserted text is present.
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

        // accept_change on the insertion clears the mark; on the deletion removes text.
        d2.accept_change(&ins_id);
        d2.accept_change(&del_id);
        let after = d2.materialize();
        assert_eq!(after.blocks[0].text, "hello there ", "accept-ins keeps text; accept-del removes 'world'");
        assert!(after.changes.is_empty(), "both changes resolved");

        // Idempotent across a second round-trip (ids volatile; compare kind+author+span).
        let exported2 = DocxCodec.export(d.as_ref()).unwrap();
        let mut d3 = Document::new();
        DocxCodec.import(&exported2, &mut d3.as_mut()).unwrap();
        let summary = |snap: &DocSnapshot| {
            let mut v: Vec<(crate::snapshot::ChangeKind, String, Option<(usize, usize)>)> = snap
                .changes
                .iter()
                .map(|c| (c.kind, c.author.clone(), c.range.as_ref().map(|(_, s, e)| (*s, *e))))
                .collect();
            v.sort_by(|a, b| (a.2, &a.1).cmp(&(b.2, &b.1)));
            v
        };
        let mut d_re = Document::new();
        DocxCodec.import(&exported, &mut d_re.as_mut()).unwrap();
        assert_eq!(summary(&d_re.materialize()), summary(&d3.materialize()), "changes stable across a second round-trip");
    }

    #[test]
    fn preserves_unmodeled_parts() {
        let base = minimal_docx_styled("", "<w:p><w:r><w:t>hi</w:t></w:r></w:p>");
        let mut c = DocxContainer::read(&base).unwrap();
        c.set("customXml/item1.xml", b"<root>custom</root>".to_vec());
        let docx = c.write().unwrap();

        let mut d = Document::new();
        DocxCodec.import(&docx, &mut d.as_mut()).unwrap();
        let exported = DocxCodec.export(d.as_ref()).unwrap();

        let out = DocxContainer::read(&exported).unwrap();
        assert_eq!(
            out.get("customXml/item1.xml"),
            Some(&b"<root>custom</root>"[..]),
            "unmodeled part must round-trip byte-identically"
        );
        assert!(out.get("word/document.xml").is_some(), "body regenerated");
        // styles.xml from the source survives verbatim.
        assert_eq!(out.get("word/styles.xml"), c.get("word/styles.xml"));
    }

    #[test]
    fn columns_roundtrip() {
        // Author a 2-column body with a gap, export to .docx, re-import: the column
        // setting survives, and the sectPr carries a `w:cols w:num="2" w:space="…"`.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "flows across columns");
        d.set_columns(2, Some(24.0)); // 24 px gap → 360 twips

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let out = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(out.get("word/document.xml").unwrap()).unwrap();
        assert!(document.contains("<w:sectPr>"), "sectPr emitted for columns: {document}");
        assert!(document.contains(r#"<w:cols w:num="2""#), "w:cols num=2 in sectPr: {document}");
        assert!(document.contains(r#"w:space="360""#), "w:cols space=360 twips (24px): {document}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        let (count, gap) = d2.columns();
        assert_eq!(count, 2, "column count survives .docx round-trip");
        assert_eq!(gap, Some(24.0), "column gap (px) survives .docx round-trip");
        assert_eq!(d2.materialize().columns, 2, "snapshot surfaces the column count");
    }

    #[test]
    fn columns_added_to_existing_header_sectpr() {
        // A document that already needs a sectPr (it has a header) must get `w:cols`
        // ADDED to that same sectPr, not a second one.
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "body");
        let h = d.insert_header_block(None, "paragraph");
        d.splice_header_text(&h, 0, 0, "Header");
        d.set_columns(3, None);

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let out = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(out.get("word/document.xml").unwrap()).unwrap();
        assert_eq!(document.matches("<w:sectPr>").count(), 1, "exactly one sectPr: {document}");
        assert!(document.contains("<w:headerReference"), "header reference kept: {document}");
        assert!(document.contains(r#"<w:cols w:num="3""#), "columns folded into the sectPr: {document}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        assert_eq!(d2.columns().0, 3, "column count survives alongside the header");
        assert_eq!(d2.header()[0].text, "Header", "header survives alongside columns");
    }

    #[test]
    fn single_column_emits_no_cols() {
        // The default single-column document must not emit a `w:cols` (nor force a
        // sectPr when nothing else needs one).
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "plain");

        let exported = DocxCodec.export(d.as_ref()).unwrap();
        let out = DocxContainer::read(&exported).unwrap();
        let document = std::str::from_utf8(out.get("word/document.xml").unwrap()).unwrap();
        assert!(!document.contains("<w:cols"), "no w:cols for a single-column doc: {document}");

        let mut d2 = Document::new();
        DocxCodec.import(&exported, &mut d2.as_mut()).unwrap();
        assert_eq!(d2.columns(), (1, None), "single-column default round-trips");
    }
}
