//! Document properties ⇄ OOXML `docProps/core.xml` (+ `app.xml` generator).
//! OOXML splits document metadata across two parts:
//!   * `docProps/core.xml` — `cp:coreProperties` with `dc:title`, `dc:creator`,
//!     `dc:subject`, `cp:keywords`, `dcterms:created`, `dcterms:modified`.
//!   * `docProps/app.xml` — `Properties > Application` (the generator string).
//! This maps those onto the canonical `meta.props` document properties, both
//! directions, preserving unmodeled `core.xml` children verbatim on export.

use crate::model::{doc_prop, DocMut, DocRef};
use crate::schema::*;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeMap;

/// The element local-name (after any `prefix:`).
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

/// Map a `core.xml` element local-name onto a canonical document-property key.
/// `created`/`modified` come from `dcterms:*`; `keywords` from `cp:keywords`.
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

/// Collect the text content of every element matching `prop_of` into the doc's
/// `meta.props` map. Shared engine for `core.xml`/`app.xml`.
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
                if current.is_some() {
                    if let Ok(s) = t.unescape() {
                        text.push_str(&s);
                    }
                }
            }
            Ok(Event::End(e)) => {
                if let Some(prop) = prop_of(local_name(e.name().as_ref())) {
                    if current == Some(prop) {
                        if !text.is_empty() {
                            doc.set_doc_prop(prop, &text);
                        }
                        current = None;
                        text.clear();
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

/// Import `docProps/core.xml` into document properties.
pub(super) fn import_core(xml: &[u8], doc: &mut DocMut) {
    import_elements(xml, doc, core_prop_for_element);
}

/// Import the generator (`Application`) from `docProps/app.xml`.
pub(super) fn import_app(xml: &[u8], doc: &mut DocMut) {
    import_elements(xml, doc, |local| if local == b"Application" { Some(PROP_GENERATOR) } else { None });
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
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
const CORE_MODELED: [&[u8]; 6] = [b"title", b"creator", b"subject", b"keywords", b"created", b"modified"];

/// Serialize the modeled `core.xml` children from the document's properties, in
/// canonical key order. `dcterms:created`/`modified` carry the W3CDTF type attr
/// Word writes, so the value round-trips as a real timestamp.
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
    emit(PROP_CREATED, r#"<dcterms:created xsi:type="dcterms:W3CDTF">"#, "</dcterms:created>");
    emit(PROP_MODIFIED, r#"<dcterms:modified xsi:type="dcterms:W3CDTF">"#, "</dcterms:modified>");
    out
}

/// Re-emit a start tag verbatim (name + attributes), self-closing if `empty`.
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

/// The inner XML of `cp:coreProperties` with the modeled children stripped, so an
/// existing `core.xml`'s unmodeled fields (lastModifiedBy, revision, …) survive.
fn core_unmodeled_children(xml: &[u8]) -> String {
    unmodeled_inner(xml, "coreProperties", &CORE_MODELED)
}

/// Strip the elements whose local-name is in `modeled_names` from the inner XML
/// of the `container` element, returning the surviving inner XML verbatim.
fn unmodeled_inner(xml: &[u8], container: &str, modeled_names: &[&[u8]]) -> String {
    let modeled: BTreeMap<&[u8], ()> = modeled_names.iter().map(|k| (*k, ())).collect();
    let inner = match element_inner(xml, container) {
        Some(i) => i,
        None => return String::new(),
    };
    let mut reader = Reader::from_reader(inner.as_slice());
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out: Vec<u8> = Vec::new();
    let mut skip_depth = 0usize;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let is_modeled = modeled.contains_key(local_name(e.name().as_ref()));
                if skip_depth > 0 {
                    skip_depth += 1;
                } else if is_modeled {
                    skip_depth = 1;
                } else {
                    append_start(&mut out, &e, false);
                }
            }
            Ok(Event::Empty(e)) => {
                let is_modeled = modeled.contains_key(local_name(e.name().as_ref()));
                if skip_depth == 0 && !is_modeled {
                    append_start(&mut out, &e, true);
                }
            }
            Ok(Event::End(e)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
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
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Inner XML of the first element whose local-name (after a `prefix:`) is
/// `local`. Shallow extraction via substring search; sufficient for the flat
/// metadata parts.
fn element_inner(xml: &[u8], local: &str) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(xml);
    // Find the opening tag: the local name preceded by `<` or `<prefix:`.
    let needle = local;
    let mut search_from = 0;
    let (open_tag_start, open_tag_gt) = loop {
        let rel = text[search_from..].find(needle)?;
        let pos = search_from + rel;
        // Validate it's an element name: preceded by `<` or `:` (after `<prefix`).
        let before = text[..pos].rfind('<')?;
        let between = &text[before + 1..pos];
        // `between` must be empty or a `prefix:` (alnum + trailing colon).
        let is_tag = between.is_empty() || (between.ends_with(':') && between[..between.len() - 1].chars().all(|c| c.is_alphanumeric()));
        // The char after the name must end the name (space, `>`, `/`).
        let after_ch = text[pos + needle.len()..].chars().next();
        let name_ends = matches!(after_ch, Some(' ') | Some('>') | Some('/') | Some('\t') | Some('\n') | Some('\r'));
        if is_tag && name_ends {
            let gt = text[before..].find('>')? + before;
            break (before, gt);
        }
        search_from = pos + needle.len();
    };
    // Self-closing?
    if text.as_bytes().get(open_tag_gt.wrapping_sub(1)) == Some(&b'/') {
        let _ = open_tag_start;
        return Some(Vec::new());
    }
    let inner_start = open_tag_gt + 1;
    // The close tag: `</...local>`.
    let close_needle = format!("{needle}>");
    let mut from = inner_start;
    loop {
        let rel = text[from..].find(&close_needle)?;
        let pos = from + rel;
        let lt = text[..pos].rfind('<')?;
        if &text[lt..lt + 2] == "</" {
            return Some(text[inner_start..lt].as_bytes().to_vec());
        }
        from = pos + close_needle.len();
    }
}

/// Build `docProps/core.xml` for export from the doc's properties, merging in any
/// unmodeled children of the preserved original. Returns `None` only when there
/// are no modeled core properties *and* no original part (so we don't synthesise
/// an empty part for documents that never had one).
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

/// Build `docProps/app.xml` for export. Only the generator (`Application`) is
/// modeled; when an original exists its other `Properties` children are preserved
/// while `Application` is regenerated from the doc property. Returns `None` when
/// there's no generator *and* no original part.
pub(super) fn export_app(doc: &DocRef, original: Option<&[u8]>) -> Option<Vec<u8>> {
    let generator = doc_prop(doc.doc, doc.c, PROP_GENERATOR);
    if generator.is_none() && original.is_none() {
        return None;
    }
    let preserved = original.map(|o| app_unmodeled_children(o)).unwrap_or_default();
    let app = generator.map(|g| format!("<Application>{}</Application>", escape_text(&g))).unwrap_or_default();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties {APP_NS}>{preserved}{app}</Properties>"#
    );
    Some(xml.into_bytes())
}

/// The inner XML of `Properties` with the modeled `Application` child stripped,
/// so an existing app.xml's other fields (Company, Pages, …) survive.
fn app_unmodeled_children(xml: &[u8]) -> String {
    unmodeled_inner(xml, "Properties", &[b"Application"])
}
