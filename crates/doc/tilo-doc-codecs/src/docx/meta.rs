//! Document properties ⇄ OOXML `docProps/core.xml` (+ `app.xml` generator). OOXML splits
//! metadata across two parts: `core.xml`'s `cp:coreProperties` (title/creator/subject/
//! keywords/created/modified) and `app.xml`'s `Properties > Application` generator string.
//! This maps those onto `meta.props`, preserving unmodeled `core.xml` children on export.

use crate::office_meta::{import_elements, unmodeled_inner};
use crate::xml_escape::escape_text;
use tilo_doc::model::{DocMut, DocRef, doc_prop};
use tilo_doc::schema::{
    PROP_AUTHOR, PROP_CREATED, PROP_GENERATOR, PROP_KEYWORDS, PROP_MODIFIED, PROP_SUBJECT,
    PROP_TITLE,
};

/// Map a `core.xml` element local-name onto a canonical document-property key.
fn core_prop_for_element(local: &[u8]) -> Option<&'static str> {
    match local {
        b"title" => Some(PROP_TITLE),
        b"creator" => Some(PROP_AUTHOR),
        b"subject" => Some(PROP_SUBJECT),
        b"keywords" => Some(PROP_KEYWORDS),
        b"created" => Some(PROP_CREATED),
        b"modified" => Some(PROP_MODIFIED),
        _ => None,
    }
}

pub(super) fn import_core(xml: &[u8], doc: &mut DocMut) {
    import_elements(xml, doc, core_prop_for_element);
}

pub(super) fn import_app(xml: &[u8], doc: &mut DocMut) {
    import_elements(xml, doc, |local| {
        if local == b"Application" {
            Some(PROP_GENERATOR)
        } else {
            None
        }
    });
}

const CORE_NS: &str = concat!(
    r#"xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" "#,
    r#"xmlns:dc="http://purl.org/dc/elements/1.1/" "#,
    r#"xmlns:dcterms="http://purl.org/dc/terms/" "#,
    r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#,
);

const APP_NS: &str =
    r#"xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties""#;

/// The modeled child elements (by local name) of `cp:coreProperties` we own.
const CORE_MODELED: [&[u8]; 6] = [
    b"title",
    b"creator",
    b"subject",
    b"keywords",
    b"created",
    b"modified",
];

/// Serialize the modeled `core.xml` children in canonical key order. `dcterms:created`/
/// `modified` carry the W3CDTF type attr Word writes, so the value round-trips as a timestamp.
fn core_modeled_children(doc: &DocRef) -> String {
    let mut out = String::new();
    let mut emit = |key: &str, open: &str, close: &str| {
        if let Some(val) = doc_prop(doc.doc, doc.c, key) {
            out.push_str(open);
            out.push_str(&escape_text(&val));
            out.push_str(close);
        }
    };
    emit(PROP_TITLE, "<dc:title>", "</dc:title>");
    emit(PROP_AUTHOR, "<dc:creator>", "</dc:creator>");
    emit(PROP_SUBJECT, "<dc:subject>", "</dc:subject>");
    emit(PROP_KEYWORDS, "<cp:keywords>", "</cp:keywords>");
    emit(
        PROP_CREATED,
        r#"<dcterms:created xsi:type="dcterms:W3CDTF">"#,
        "</dcterms:created>",
    );
    emit(
        PROP_MODIFIED,
        r#"<dcterms:modified xsi:type="dcterms:W3CDTF">"#,
        "</dcterms:modified>",
    );
    out
}

/// The inner XML of `cp:coreProperties` with modeled children stripped, so an existing
/// `core.xml`'s unmodeled fields (lastModifiedBy, revision, …) survive.
fn core_unmodeled_children(xml: &[u8]) -> String {
    unmodeled_inner(xml, "coreProperties", &CORE_MODELED)
}

/// Build `docProps/core.xml` for export, merging in unmodeled children of the preserved
/// original. `None` only when there are no modeled properties *and* no original part.
pub(super) fn export_core(doc: &DocRef, original: Option<&[u8]>) -> Option<Vec<u8>> {
    let modeled = core_modeled_children(doc);
    let preserved = original.map(core_unmodeled_children).unwrap_or_default();
    if modeled.is_empty() && original.is_none() {
        return None;
    }
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties {CORE_NS}>{preserved}{modeled}</cp:coreProperties>"#
    );
    Some(xml.into_bytes())
}

/// Build `docProps/app.xml` for export; only the generator is modeled, an original's
/// other `Properties` children are preserved. `None` when there's no generator and no original.
pub(super) fn export_app(doc: &DocRef, original: Option<&[u8]>) -> Option<Vec<u8>> {
    let generator = doc_prop(doc.doc, doc.c, PROP_GENERATOR);
    if generator.is_none() && original.is_none() {
        return None;
    }
    let preserved = original.map(app_unmodeled_children).unwrap_or_default();
    let app = generator
        .map(|g| format!("<Application>{}</Application>", escape_text(&g)))
        .unwrap_or_default();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties {APP_NS}>{preserved}{app}</Properties>"#
    );
    Some(xml.into_bytes())
}

/// The inner XML of `Properties` with the modeled `Application` child stripped.
fn app_unmodeled_children(xml: &[u8]) -> String {
    unmodeled_inner(xml, "Properties", &[b"Application"])
}
