//! Document properties ⇄ OOXML `docProps/core.xml` (+ `app.xml` generator). OOXML splits
//! metadata across two parts: `core.xml`'s `cp:coreProperties` (title/creator/subject/
//! keywords/created/modified) and `app.xml`'s `Properties > Application` generator string.
//! This maps those onto `meta.props`, preserving unmodeled `core.xml` children on export.

use crate::office_meta::{import_elements, unmodeled_inner};
use crate::office_props::ooxml::{CORE_NS, core_modeled_children, core_prop_for_element};
use crate::xml_escape::escape_text;
use tilo_doc::model::{DocMut, DocRef, doc_prop};
use tilo_doc::schema::PROP_GENERATOR;

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

/// The inner XML of `cp:coreProperties` with modeled children stripped, so an existing
/// `core.xml`'s unmodeled fields (lastModifiedBy, revision, …) survive.
fn core_unmodeled_children(xml: &[u8]) -> String {
    unmodeled_inner(xml, "coreProperties", &CORE_MODELED)
}

/// Build `docProps/core.xml` for export, merging in unmodeled children of the preserved
/// original. `None` only when there are no modeled properties *and* no original part.
pub(super) fn export_core(doc: &DocRef, original: Option<&[u8]>) -> Option<Vec<u8>> {
    let modeled = core_modeled_children(|key| doc_prop(doc.doc, doc.c, key));
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
