//! Page headers and footers survive a `.docx` import → export → import, and the exported
//! package stays internally consistent: every relationship id `word/document.xml` uses
//! resolves to a relationship of the right type, every relationship target exists, and
//! `[Content_Types].xml` covers every part without naming parts that are gone.
//!
//! The fixtures are shaped like Word's output: relationship ids `rIdN`, a first-page and a
//! default header in separate parts, and content-type overrides for each part.

use quick_xml::events::Event;
use quick_xml::events::attributes::Attribute;
use quick_xml::reader::Reader;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};
use tilo_doc::Document;
use tilo_doc_codecs::{DocxCodec, FormatProvider};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const HEADER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
const HEADER_CT: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml";
const FOOTER_CT: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml";
const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

/// Header text as Word wrote it, and as the model must hold it.
const HEADER_RAW: &str = "Quarterly Report R&amp;D";
const HEADER_TEXT: &str = "Quarterly Report R&D";
const FIRST_PAGE_HEADER_TEXT: &str = "Cover page";
const SECTION_ONE_HEADER_TEXT: &str = "Section one";
const FOOTER_TEXT: &str = "Confidential \u{2013} page ";

fn zip_of(parts: &[(&str, String)]) -> Vec<u8> {
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, body) in parts {
        zw.start_file(*name, opts).unwrap();
        zw.write_all(body.as_bytes()).unwrap();
    }
    zw.finish().unwrap().into_inner()
}

/// Every part of a package, by name, in archive order.
fn parts_of(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    (0..archive.len())
        .map(|i| {
            let mut f = archive.by_index(i).unwrap();
            let mut out = Vec::new();
            f.read_to_end(&mut out).unwrap();
            (f.name().to_string(), out)
        })
        .collect()
}

fn text_of(parts: &[(String, Vec<u8>)], name: &str) -> String {
    let bytes = &parts
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| panic!("part {name} missing"))
        .1;
    String::from_utf8(bytes.clone()).unwrap()
}

fn import(bytes: &[u8]) -> Document {
    let mut d = Document::new();
    DocxCodec
        .import(
            bytes,
            &mut d.as_mut(),
            &mut tilo_content::ContentInputs::new(),
        )
        .unwrap();
    d
}

fn export(d: &Document) -> Vec<u8> {
    DocxCodec
        .export(d.as_ref(), &tilo_content::NoContent)
        .unwrap()
}

fn hdr_part(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr {W}>{inner}</w:hdr>"#
    )
}

fn ftr_part(inner: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:ftr {W}>{inner}</w:ftr>"#
    )
}

fn run_para(text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:pStyle w:val="Header"/></w:pPr><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#
    )
}

/// A footer paragraph with a complex `PAGE` field, the way Word writes page numbers.
fn page_footer_para() -> String {
    concat!(
        r#"<w:p><w:r><w:t xml:space="preserve">Confidential &#8211; page </w:t></w:r>"#,
        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r>"#,
        r#"<w:r><w:instrText xml:space="preserve"> PAGE   \* MERGEFORMAT </w:instrText></w:r>"#,
        r#"<w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
        r#"<w:r><w:t>1</w:t></w:r>"#,
        r#"<w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
    )
    .to_string()
}

/// A Word-shaped package. The default header lives in `word/header2.xml` under `rId8`, a
/// first-page header in `word/header1.xml` under `rId7` (with a part-relationships file of
/// its own), and the default footer in `word/footer1.xml` under `rId9`. With
/// `section_break`, a paragraph also ends an earlier section whose own default header is
/// `word/header3.xml` under `rId12`.
fn word_docx_with_first_page_header(section_break: bool) -> Vec<u8> {
    let (extra_override, extra_rel, brk) = if section_break {
        (
            r#"<Override PartName="/word/header3.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>"#,
            r#"<Relationship Id="rId12" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header3.xml"/>"#,
            r#"<w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rId12"/><w:type w:val="nextPage"/></w:sectPr></w:pPr><w:r><w:t>End of section one</w:t></w:r></w:p>"#,
        )
    } else {
        ("", "", "")
    };
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/header2.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/><Override PartName="/word/fontTable.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.fontTable+xml"/>{extra_override}</Types>"#
    );
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId8" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/><Relationship Id="rId11" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/fontTable" Target="fontTable.xml"/>{extra_rel}</Relationships>"#
    );
    let header1_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {W}><w:body><w:p><w:r><w:t>Body text</w:t></w:r></w:p>{brk}<w:sectPr w:rsidR="00B4217A"><w:headerReference w:type="default" r:id="rId8"/><w:headerReference w:type="first" r:id="rId7"/><w:footerReference w:type="default" r:id="rId9"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/><w:titlePg/><w:docGrid w:linePitch="360"/></w:sectPr></w:body></w:document>"#
    );
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles {W}/>"#
    );
    let settings = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings {W}><w:zoom w:percent="100"/></w:settings>"#
    );
    let font_table = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:fonts {W}><w:font w:name="Calibri"/></w:fonts>"#
    );
    let mut parts = vec![
        ("[Content_Types].xml", content_types),
        ("_rels/.rels", root_rels.to_string()),
        ("word/document.xml", document),
        ("word/_rels/document.xml.rels", doc_rels),
        (
            "word/header1.xml",
            hdr_part(&run_para(FIRST_PAGE_HEADER_TEXT)),
        ),
        ("word/_rels/header1.xml.rels", header1_rels.to_string()),
        ("word/header2.xml", hdr_part(&run_para(HEADER_RAW))),
        ("word/footer1.xml", ftr_part(&page_footer_para())),
        ("word/styles.xml", styles),
        ("word/settings.xml", settings),
        ("word/fontTable.xml", font_table),
    ];
    if section_break {
        parts.push((
            "word/header3.xml",
            hdr_part(&run_para(SECTION_ONE_HEADER_TEXT)),
        ));
    }
    zip_of(&parts)
}

/// The simplest Word shape: one default header and footer, already named `header1.xml` /
/// `footer1.xml`, referenced under Word's `rId6`/`rId7`.
fn word_docx_default_only() -> Vec<u8> {
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/footer1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rId7" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#;
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {W}><w:body><w:p><w:r><w:t>Body text</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId6"/><w:footerReference w:type="default" r:id="rId7"/></w:sectPr></w:body></w:document>"#
    );
    zip_of(&[
        ("[Content_Types].xml", content_types.to_string()),
        ("_rels/.rels", root_rels.to_string()),
        ("word/document.xml", document),
        ("word/_rels/document.xml.rels", doc_rels.to_string()),
        ("word/header1.xml", hdr_part(&run_para(HEADER_RAW))),
        ("word/footer1.xml", ftr_part(&page_footer_para())),
    ])
}

fn value(a: &Attribute) -> String {
    quick_xml::escape::unescape(std::str::from_utf8(&a.value).unwrap())
        .unwrap()
        .into_owned()
}

/// Every element of `xml` named `name`, as its attribute map.
fn elements(xml: &str, name: &[u8]) -> Vec<BTreeMap<String, String>> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == name => {
                out.push(
                    e.attributes()
                        .flatten()
                        .map(|a| {
                            (
                                String::from_utf8(a.key.as_ref().to_vec()).unwrap(),
                                value(&a),
                            )
                        })
                        .collect(),
                );
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// `(element, attribute, id)` for every relationship-namespace attribute in `xml`.
fn relationship_uses(xml: &str) -> Vec<(String, String, String)> {
    let mut reader = Reader::from_str(xml);
    let mut prefixes: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) | Event::Empty(e) => {
                for a in e.attributes().flatten() {
                    let key = a.key.as_ref();
                    if let Some(p) = key.strip_prefix(b"xmlns:")
                        && value(&a) == REL_NS
                    {
                        prefixes.insert(p.to_vec());
                    }
                }
                for a in e.attributes().flatten() {
                    let key = a.key.as_ref();
                    if let Some(colon) = key.iter().position(|&b| b == b':')
                        && prefixes.contains(&key[..colon])
                    {
                        out.push((
                            String::from_utf8(e.name().as_ref().to_vec()).unwrap(),
                            String::from_utf8(key.to_vec()).unwrap(),
                            value(&a),
                        ));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// The part a `.rels` file describes relationships for: `word/_rels/x.xml.rels` → `word/x.xml`.
fn rels_source(rels_name: &str) -> String {
    let (dir, file) = rels_name.rsplit_once('/').unwrap();
    let dir = dir.strip_suffix("_rels").unwrap().trim_end_matches('/');
    let file = file.strip_suffix(".rels").unwrap();
    if dir.is_empty() {
        file.to_string()
    } else {
        format!("{dir}/{file}")
    }
}

/// Resolve a relationship `Target` against its source part into a package part name.
fn resolve_target(source: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut segs: Vec<&str> = source.split('/').collect();
    segs.pop();
    for seg in target.split('/') {
        match seg {
            ".." => {
                segs.pop();
            }
            "." | "" => {}
            s => segs.push(s),
        }
    }
    segs.join("/")
}

/// Content types: every `Override` names an existing part, once, and every part is typed.
/// Returns the overrides as part name → content type.
fn assert_content_types(parts: &[(String, Vec<u8>)]) -> BTreeMap<String, String> {
    let names: BTreeSet<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
    let ct = text_of(parts, "[Content_Types].xml");
    let overrides: BTreeMap<String, String> = elements(&ct, b"Override")
        .into_iter()
        .map(|o| {
            (
                o["PartName"].trim_start_matches('/').to_string(),
                o["ContentType"].clone(),
            )
        })
        .collect();
    let override_count = elements(&ct, b"Override").len();
    assert_eq!(overrides.len(), override_count, "duplicate overrides: {ct}");
    let defaults: BTreeSet<String> = elements(&ct, b"Default")
        .into_iter()
        .map(|d| d["Extension"].to_ascii_lowercase())
        .collect();
    for part in overrides.keys() {
        assert!(
            names.contains(part.as_str()),
            "content-type override for missing part {part}: {ct}"
        );
    }
    for name in &names {
        if *name == "[Content_Types].xml" {
            continue;
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        assert!(
            overrides.contains_key(*name) || ext.is_some_and(|e| defaults.contains(&e)),
            "part {name} has no content type: {ct}"
        );
    }
    overrides
}

/// Relationship id → (type, target part) of each relationships part, by relationships part name.
type PackageRels = BTreeMap<String, BTreeMap<String, (String, String)>>;

/// Relationships: unique ids, no duplicate (type, target), every internal target exists, and
/// every relationships part belongs to an existing part.
fn assert_relationships(parts: &[(String, Vec<u8>)]) -> PackageRels {
    let names: BTreeSet<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
    let mut all_rels = PackageRels::new();
    let is_rels = |n: &&str| {
        std::path::Path::new(n)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rels"))
    };
    for name in names.iter().copied().filter(is_rels) {
        let source = rels_source(name);
        assert!(
            source.is_empty() || names.contains(source.as_str()),
            "relationships file {name} for missing part {source}"
        );
        let xml = text_of(parts, name);
        let mut by_id = BTreeMap::new();
        let mut pairs = BTreeSet::new();
        for r in elements(&xml, b"Relationship") {
            let id = r["Id"].clone();
            let ty = r["Type"].clone();
            let target = r["Target"].clone();
            let external = r.get("TargetMode").is_some_and(|m| m == "External");
            assert!(
                pairs.insert((ty.clone(), target.clone())),
                "duplicate relationship {ty} → {target} in {name}: {xml}"
            );
            if !external {
                let part = resolve_target(&source, &target);
                assert!(
                    names.contains(part.as_str()),
                    "{name}: relationship {id} targets missing part {part}: {xml}"
                );
                assert!(
                    by_id.insert(id.clone(), (ty, part)).is_none(),
                    "duplicate relationship id {id} in {name}: {xml}"
                );
            }
        }
        all_rels.insert(name.to_string(), by_id);
    }
    all_rels
}

/// Assert the package is self-consistent, and return the header/footer part names that
/// `word/document.xml` references, in reference order.
fn assert_consistent_package(bytes: &[u8]) -> Vec<String> {
    let parts = parts_of(bytes);
    let names: Vec<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
    let unique: BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(names.len(), unique.len(), "duplicate part names: {names:?}");
    let overrides = assert_content_types(&parts);
    let all_rels = assert_relationships(&parts);

    // Every relationship id the document body uses resolves; header/footer references resolve
    // to a relationship of the matching type whose target carries the matching content type.
    let document = text_of(&parts, "word/document.xml");
    let doc_rels = all_rels
        .get("word/_rels/document.xml.rels")
        .cloned()
        .unwrap_or_default();
    let mut referenced = Vec::new();
    for (element, key, id) in relationship_uses(&document) {
        let (ty, part) = doc_rels
            .get(&id)
            .unwrap_or_else(|| panic!("<{element} {key}=\"{id}\"> does not resolve: {doc_rels:?}"));
        let expected = match element.as_str() {
            "w:headerReference" => Some((HEADER_REL_TYPE, HEADER_CT)),
            "w:footerReference" => Some((FOOTER_REL_TYPE, FOOTER_CT)),
            _ => None,
        };
        if let Some((rel_type, content_type)) = expected {
            assert_eq!(ty, rel_type, "{element} {id} has relationship type {ty}");
            assert_eq!(
                overrides.get(part).map(String::as_str),
                Some(content_type),
                "{element} {id} → {part} is not typed {content_type}: {overrides:?}"
            );
            referenced.push(part.clone());
        }
    }

    // No header/footer part is carried along without a relationship that reaches it.
    let hf_targets: BTreeSet<&str> = all_rels
        .values()
        .flat_map(BTreeMap::values)
        .filter(|(ty, _)| ty == HEADER_REL_TYPE || ty == FOOTER_REL_TYPE)
        .map(|(_, part)| part.as_str())
        .collect();
    for (part, content_type) in &overrides {
        if content_type == HEADER_CT || content_type == FOOTER_CT {
            assert!(
                hf_targets.contains(part.as_str()),
                "orphaned header/footer part {part}; header/footer relationships reach {hf_targets:?}"
            );
        }
    }
    referenced
}

fn assert_footer_has_page_field(d: &Document) {
    let footer = d.footer();
    assert_eq!(footer.len(), 1, "one footer paragraph: {footer:?}");
    let expected = format!("{FOOTER_TEXT}{}", tilo_doc::schema::FIELD_PLACEHOLDER);
    assert_eq!(footer[0].text, expected, "footer text preserved exactly");
    let at = FOOTER_TEXT.chars().count();
    assert!(
        footer[0]
            .marks
            .iter()
            .any(|m| m.name == tilo_doc::schema::MARK_FIELD
                && m.start == at
                && m.end == at.saturating_add(1)
                && m.value.as_str() == Some(tilo_doc::schema::FIELD_PAGE_NUMBER)),
        "page-number field kept: {:?}",
        footer[0].marks
    );
}

fn header_texts(d: &Document) -> Vec<String> {
    d.header().into_iter().map(|b| b.text).collect()
}

const HEADER_EDIT: &str = " (revised)";

/// Edit both page regions through the model: text appended to the header, the footer's first
/// word made bold (its text and field stay as they were).
fn edit_regions(d: &mut Document) {
    let header = d.header_blocks_in_order();
    let first = header.first().expect("a header block");
    let len = d.header()[0].text.chars().count();
    d.splice_header_text(first, len, 0, HEADER_EDIT);
    let footer = d.footer_blocks_in_order();
    let first = footer.first().expect("a footer block");
    d.as_mut()
        .add_mark_in(tilo_doc::Region::Footer, first, 0, 3, "strong", true);
}

/// Import → export → import (twice), asserting the default header and footer text survive
/// exactly, every exported package is consistent, and `expected_refs` header/footer parts are
/// referenced from the section.
fn assert_word_roundtrip(fixture: &[u8], expected_refs: usize) {
    let d1 = import(fixture);
    assert_eq!(header_texts(&d1), [HEADER_TEXT], "imported default header");
    assert_footer_has_page_field(&d1);

    let exported = export(&d1);
    let referenced = assert_consistent_package(&exported);
    assert_eq!(referenced.len(), expected_refs, "{referenced:?}");
    let d2 = import(&exported);
    assert_eq!(header_texts(&d2), [HEADER_TEXT], "header after re-import");
    assert_footer_has_page_field(&d2);
    assert_eq!(d2.materialize().blocks[0].text, "Body text");

    let exported2 = export(&d2);
    assert_eq!(assert_consistent_package(&exported2).len(), expected_refs);
    let d3 = import(&exported2);
    assert_eq!(
        header_texts(&d3),
        [HEADER_TEXT],
        "header after a second round-trip"
    );
    assert_footer_has_page_field(&d3);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn word_headers_and_footers_survive_docx_roundtrip() {
    // Phase `word_header_and_footer_survive_docx_roundtrip`.
    {
        assert_word_roundtrip(&word_docx_default_only(), 2);
    }
    // Phase `word_default_header_survives_alongside_first_page_header`.
    {
        // The default header and footer, plus the carried first-page header.
        assert_word_roundtrip(&word_docx_with_first_page_header(false), 3);
    }
    // Phase `unedited_header_and_footer_keep_their_parts`.
    {
        // Regions the model holds unedited are written back as imported, parts, relationships and
        // everything the model does not hold included.
        let fixture = word_docx_with_first_page_header(true);
        let exported = export(&import(&fixture));
        assert_consistent_package(&exported);
        let parts = parts_of(&exported);
        let original = parts_of(&fixture);
        for name in [
            "word/header1.xml",
            "word/header2.xml",
            "word/header3.xml",
            "word/footer1.xml",
        ] {
            assert_eq!(
                original.iter().find(|(n, _)| n == name),
                parts.iter().find(|(n, _)| n == name),
                "{name} kept as it was"
            );
        }
        let document = text_of(&parts, "word/document.xml");
        assert!(document.contains(r#"r:id="rId8""#) && document.contains(r#"r:id="rId9""#));
        // The section break keeps its own header too.
        assert!(document.contains(r#"r:id="rId12""#), "{document}");
        let d2 = import(&exported);
        assert_eq!(header_texts(&d2), [HEADER_TEXT]);
        assert_footer_has_page_field(&d2);
    }
    // Phase `first_page_header_and_page_setup_are_carried_through`.
    {
        let mut d = import(&word_docx_with_first_page_header(false));
        edit_regions(&mut d);
        let exported = export(&d);
        assert_consistent_package(&exported);
        let parts = parts_of(&exported);
        let document = text_of(&parts, "word/document.xml");
        let sectpr = only_sectpr(&document);
        assert!(
            sectpr.starts_with(r#"<w:sectPr w:rsidR="00B4217A">"#),
            "section start tag kept: {sectpr}"
        );
        assert!(
            sectpr.contains(r#"<w:headerReference w:type="first" r:id="rId7"/>"#),
            "first-page header reference kept: {sectpr}"
        );
        for kept in [
            r#"<w:pgSz w:w="11906" w:h="16838"/>"#,
            "<w:titlePg/>",
            "<w:docGrid",
        ] {
            assert!(sectpr.contains(kept), "{kept} kept: {sectpr}");
        }
        assert!(
            !sectpr.contains("rId8"),
            "old default header reference replaced: {sectpr}"
        );
        assert!(
            !sectpr.contains("rId9"),
            "old default footer reference replaced: {sectpr}"
        );

        // The first-page header part keeps its name, text and relationships; qdoc's default
        // header takes the part name the old default header freed.
        assert!(text_of(&parts, "word/header1.xml").contains(FIRST_PAGE_HEADER_TEXT));
        assert!(
            parts
                .iter()
                .any(|(n, _)| n == "word/_rels/header1.xml.rels")
        );
        assert!(text_of(&parts, "word/header2.xml").contains("Quarterly Report R&amp;D"));
        let rels = text_of(&parts, "word/_rels/document.xml.rels");
        assert!(
            !rels.contains(r#"Id="rId8""#),
            "old default header rel dropped: {rels}"
        );
        assert!(
            !rels.contains(r#"Id="rId9""#),
            "old default footer rel dropped: {rels}"
        );
        // Unrelated parts and their relationships are carried through untouched.
        for kept in [
            r#"<Relationship Id="rId11" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/fontTable" Target="fontTable.xml"/>"#,
            r#"<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/>"#,
        ] {
            assert!(rels.contains(kept), "{kept} kept: {rels}");
        }
    }
    // Phase `section_break_headers_follow_the_document_header`.
    {
        // A section break inside the body is kept, but the model has one header and footer for
        // the whole document: once they are edited, the break's references point at them, and the
        // earlier section's own header part and relationship go rather than dangle.
        let mut d = import(&word_docx_with_first_page_header(true));
        assert_eq!(
            header_texts(&d),
            [HEADER_TEXT],
            "the final section's default header is the imported one"
        );
        edit_regions(&mut d);
        let exported = export(&d);
        let referenced: BTreeSet<String> =
            assert_consistent_package(&exported).into_iter().collect();
        assert_eq!(referenced.len(), 3, "{referenced:?}");
        let parts = parts_of(&exported);
        let document = text_of(&parts, "word/document.xml");
        assert!(
            document.contains(
                r#"<w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rIdQdocHdr"/>"#
            ),
            "section break kept, pointing at the document header: {document}"
        );
        for (name, bytes) in &parts {
            assert!(
                !String::from_utf8_lossy(bytes).contains(SECTION_ONE_HEADER_TEXT),
                "unreferenced section header text carried in {name}"
            );
        }
        let rels = text_of(&parts, "word/_rels/document.xml.rels");
        assert!(
            !rels.contains(r#"Id="rId12""#),
            "section header rel dropped: {rels}"
        );
        let d2 = import(&exported);
        assert_eq!(header_texts(&d2), [format!("{HEADER_TEXT}{HEADER_EDIT}")]);
        assert_footer_has_page_field(&d2);
    }
}

/// The single `w:sectPr` of an exported `word/document.xml`.
fn only_sectpr(document: &str) -> &str {
    assert_eq!(
        document.matches("<w:sectPr").count(),
        1,
        "exactly one sectPr: {document}"
    );
    let start = document.find("<w:sectPr").unwrap();
    let end = document
        .find("</w:sectPr>")
        .unwrap()
        .saturating_add("</w:sectPr>".len());
    &document[start..end]
}

#[test]
fn header_removal_and_qdoc_headers_roundtrip_consistently() {
    // Phase `removing_a_word_header_drops_its_part`.
    {
        let mut d = import(&word_docx_default_only());
        for id in d.header_blocks_in_order() {
            d.as_mut().tombstone_block_in(tilo_doc::Region::Header, &id);
        }
        assert!(d.header().is_empty(), "header cleared");
        let exported = export(&d);
        let referenced = assert_consistent_package(&exported);
        assert_eq!(referenced.len(), 1, "only the footer is referenced");
        let d2 = import(&exported);
        assert!(d2.header().is_empty(), "header stays removed");
        assert_footer_has_page_field(&d2);
    }
    // Phase `qdoc_header_and_footer_roundtrip_consistently`.
    {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "Body text");
        let h = d.insert_header_block(None, "paragraph");
        d.splice_header_text(&h, 0, 0, HEADER_TEXT);
        let f = d.insert_footer_block(None, "paragraph");
        d.splice_footer_text(&f, 0, 0, FOOTER_TEXT);
        d.insert_footer_page_number(&f, FOOTER_TEXT.chars().count());

        let exported = export(&d);
        assert_eq!(assert_consistent_package(&exported).len(), 2);
        let d2 = import(&exported);
        assert_eq!(header_texts(&d2), [HEADER_TEXT]);
        assert_footer_has_page_field(&d2);

        let exported2 = export(&d2);
        assert_eq!(assert_consistent_package(&exported2).len(), 2);
        let d3 = import(&exported2);
        assert_eq!(header_texts(&d3), [HEADER_TEXT]);
        assert_footer_has_page_field(&d3);
    }
}
