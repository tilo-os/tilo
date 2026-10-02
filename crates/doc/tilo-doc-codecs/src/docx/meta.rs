//! Document properties ⇄ OOXML `docProps/core.xml` (+ `app.xml` generator). OOXML splits
//! metadata across two parts: `core.xml`'s `cp:coreProperties` (title/creator/subject/
//! keywords/created/modified) and `app.xml`'s `Properties > Application` generator string.
//! This maps those onto `meta.props`, preserving unmodeled `core.xml` children on export.

use crate::xml_escape::escape_text;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeSet;
use tilo_doc::model::{DocMut, DocRef, doc_prop};
use tilo_doc::schema::{
    PROP_AUTHOR, PROP_CREATED, PROP_GENERATOR, PROP_KEYWORDS, PROP_MODIFIED, PROP_SUBJECT,
    PROP_TITLE,
};

fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => name.get(i.saturating_add(1)..).unwrap_or(name),
        None => name,
    }
}

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

/// Collect the text content of every element matching `prop_of` into `meta.props`.
fn import_elements(xml: &[u8], doc: &mut DocMut, prop_of: impl Fn(&[u8]) -> Option<&'static str>) {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut current: Option<&'static str> = None;
    let mut text = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if let Some(prop) = prop_of(local_name(e.name().as_ref())) {
                    current = Some(prop);
                    text.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if current.is_some()
                    && let Ok(s) = t.xml10_content()
                {
                    text.push_str(&s);
                }
            }
            Ok(Event::GeneralRef(r)) if current.is_some() => {
                text.extend(crate::resolve_general_ref(&r));
            }
            Ok(Event::End(e)) => {
                if let Some(prop) = prop_of(local_name(e.name().as_ref()))
                    && current == Some(prop)
                {
                    if !text.is_empty() {
                        doc.set_doc_prop(prop, &text);
                    }
                    current = None;
                    text.clear();
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
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

fn append_start(out: &mut Vec<u8>, e: &BytesStart, empty: bool) {
    out.push(b'<');
    out.extend_from_slice(e.name().as_ref());
    for attr in e.attributes().flatten() {
        out.push(b' ');
        out.extend_from_slice(attr.key.as_ref());
        out.extend_from_slice(b"=\"");
        out.extend_from_slice(&attr.value);
        out.push(b'"');
    }
    if empty {
        out.extend_from_slice(b"/>");
    } else {
        out.push(b'>');
    }
}

/// The inner XML of `cp:coreProperties` with modeled children stripped, so an existing
/// `core.xml`'s unmodeled fields (lastModifiedBy, revision, …) survive.
fn core_unmodeled_children(xml: &[u8]) -> String {
    unmodeled_inner(xml, "coreProperties", &CORE_MODELED)
}

/// Strip elements whose local-name is in `modeled_names`, returning the surviving inner XML.
fn unmodeled_inner(xml: &[u8], container: &str, modeled_names: &[&[u8]]) -> String {
    let modeled: BTreeSet<&[u8]> = modeled_names.iter().copied().collect();
    let Some(inner) = element_inner(xml, container) else {
        return String::new();
    };
    let mut reader = Reader::from_reader(inner.as_slice());
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out: Vec<u8> = Vec::new();
    let mut skip_depth = 0usize;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let is_modeled = modeled.contains(local_name(e.name().as_ref()));
                if skip_depth > 0 {
                    skip_depth = skip_depth.saturating_add(1);
                } else if is_modeled {
                    skip_depth = 1;
                } else {
                    append_start(&mut out, &e, false);
                }
            }
            Ok(Event::Empty(e)) => {
                let is_modeled = modeled.contains(local_name(e.name().as_ref()));
                if skip_depth == 0 && !is_modeled {
                    append_start(&mut out, &e, true);
                }
            }
            Ok(Event::End(e)) => {
                if skip_depth > 0 {
                    skip_depth = skip_depth.saturating_sub(1);
                } else {
                    out.extend_from_slice(b"</");
                    out.extend_from_slice(e.name().as_ref());
                    out.push(b'>');
                }
            }
            Ok(Event::Text(t)) => {
                if skip_depth == 0 {
                    out.extend_from_slice(t.into_inner().as_ref());
                }
            }
            // Copied through still escaped: `&amp;` has to stay `&amp;` in the output.
            Ok(Event::GeneralRef(r)) => {
                if skip_depth == 0 {
                    out.push(b'&');
                    out.extend_from_slice(&r);
                    out.push(b';');
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Inner XML of the first element whose local-name is `local`. Shallow substring
/// extraction, sufficient for the flat metadata parts.
fn element_inner(xml: &[u8], local: &str) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(xml);
    // Find the opening tag: the local name preceded by `<` or `<prefix:`.
    let needle = local;
    let mut search_from = 0;
    let (open_tag_start, open_tag_gt) = loop {
        let rel = text.get(search_from..)?.find(needle)?;
        let pos = search_from.checked_add(rel)?;
        // Validate it's an element name: preceded by `<` or `:` (after `<prefix`).
        let before = text.get(..pos)?.rfind('<')?;
        let between = text.get(before.checked_add(1)?..pos)?;
        // `between` must be empty or a `prefix:` (alnum + trailing colon).
        let is_tag = between.is_empty()
            || (between.ends_with(':')
                && between
                    .get(..between.len().saturating_sub(1))?
                    .chars()
                    .all(char::is_alphanumeric));
        // The char after the name must end the name (space, `>`, `/`).
        let after_ch = text.get(pos.checked_add(needle.len())?..)?.chars().next();
        let name_ends = matches!(after_ch, Some(' ' | '>' | '/' | '\t' | '\n' | '\r'));
        if is_tag && name_ends {
            let gt = text.get(before..)?.find('>')?.checked_add(before)?;
            break (before, gt);
        }
        search_from = pos.checked_add(needle.len())?;
    };
    // Self-closing?
    if text.as_bytes().get(open_tag_gt.wrapping_sub(1)) == Some(&b'/') {
        let _ = open_tag_start;
        return Some(Vec::new());
    }
    let inner_start = open_tag_gt.checked_add(1)?;
    // The close tag: `</...local>`.
    let close_needle = format!("{needle}>");
    let mut from = inner_start;
    loop {
        let rel = text.get(from..)?.find(&close_needle)?;
        let pos = from.checked_add(rel)?;
        let lt = text.get(..pos)?.rfind('<')?;
        if text.get(lt..lt.checked_add(2)?)? == "</" {
            return Some(text.get(inner_start..lt)?.as_bytes().to_vec());
        }
        from = pos.checked_add(close_needle.len())?;
    }
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
