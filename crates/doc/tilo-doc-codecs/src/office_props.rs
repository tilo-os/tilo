//! Document-property vocabularies shared by the office codecs: how ODF and
//! OOXML name the canonical `meta.props` keys, read from and written to
//! their metadata parts by both the document and the spreadsheet codecs.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::collections::BTreeMap;

/// The element local-name (after any `prefix:`).
pub(crate) fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => name.get(i.saturating_add(1)..).unwrap_or(name),
        None => name,
    }
}

/// Collect the non-empty text content of every element matching `prop_of`, keyed by its property.
pub(crate) fn element_props(
    xml: &[u8],
    prop_of: impl Fn(&[u8]) -> Option<&'static str>,
) -> BTreeMap<String, String> {
    let mut props = BTreeMap::new();
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
                        props.insert(prop.to_string(), text.clone());
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
    props
}

/// ODF `office:meta` children (`meta.xml`).
#[cfg(any(feature = "odt", feature = "ods"))]
pub(crate) mod odf {
    use crate::xml_escape::escape_text;
    // `write!` into a `String` cannot fail; the discarded `Result` is the
    // `fmt::Write` contract, not a swallowed error.
    use std::fmt::Write as _;
    use tilo_doc::schema::{
        DOC_PROP_KEYS, PROP_AUTHOR, PROP_CREATED, PROP_GENERATOR, PROP_KEYWORDS, PROP_MODIFIED,
        PROP_SUBJECT, PROP_TITLE,
    };

    /// Map an ODF element local-name (within `office:meta`) onto a canonical document-property key.
    pub(crate) fn prop_for_element(local: &[u8]) -> Option<&'static str> {
        match local {
            b"title" => Some(PROP_TITLE),
            b"creator" => Some(PROP_AUTHOR),
            b"subject" => Some(PROP_SUBJECT),
            b"keyword" => Some(PROP_KEYWORDS),
            b"date" => Some(PROP_MODIFIED),
            b"creation-date" => Some(PROP_CREATED),
            b"generator" => Some(PROP_GENERATOR),
            _ => None,
        }
    }

    /// The ODF element (with prefix) a canonical property maps onto inside `office:meta`.
    fn element_for_prop(prop: &str) -> Option<&'static str> {
        match prop {
            PROP_TITLE => Some("dc:title"),
            PROP_AUTHOR => Some("dc:creator"),
            PROP_SUBJECT => Some("dc:subject"),
            PROP_KEYWORDS => Some("meta:keyword"),
            PROP_MODIFIED => Some("dc:date"),
            PROP_CREATED => Some("meta:creation-date"),
            PROP_GENERATOR => Some("meta:generator"),
            _ => None,
        }
    }

    /// Serialize the modeled child elements (`dc:title`, …) from `prop`'s values, in canonical key order.
    pub(crate) fn modeled_children(prop: impl Fn(&str) -> Option<String>) -> String {
        let mut out = String::new();
        for key in DOC_PROP_KEYS {
            let Some(val) = prop(key) else {
                continue;
            };
            let Some(el) = element_for_prop(key) else {
                continue;
            };
            let _ = write!(out, "<{el}>{}</{el}>", escape_text(&val));
        }
        out
    }

    /// The XML namespaces a freshly-generated `meta.xml` declares.
    pub(crate) const META_NS: &str = concat!(
        r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" "#,
        r#"xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0" "#,
        r#"xmlns:dc="http://purl.org/dc/elements/1.1/""#,
    );
}

/// OOXML `cp:coreProperties` children (`docProps/core.xml`).
#[cfg(any(feature = "docx", feature = "xlsx"))]
pub(crate) mod ooxml {
    use crate::xml_escape::escape_text;
    use tilo_doc::schema::{
        PROP_AUTHOR, PROP_CREATED, PROP_KEYWORDS, PROP_MODIFIED, PROP_SUBJECT, PROP_TITLE,
    };

    /// Map a `core.xml` element local-name onto a canonical document-property key.
    pub(crate) fn core_prop_for_element(local: &[u8]) -> Option<&'static str> {
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

    pub(crate) const CORE_NS: &str = concat!(
        r#"xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" "#,
        r#"xmlns:dc="http://purl.org/dc/elements/1.1/" "#,
        r#"xmlns:dcterms="http://purl.org/dc/terms/" "#,
        r#"xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance""#,
    );

    /// Serialize the modeled `core.xml` children in canonical key order. `dcterms:created`/
    /// `modified` carry the W3CDTF type attr Word writes, so the value round-trips as a timestamp.
    pub(crate) fn core_modeled_children(prop: impl Fn(&str) -> Option<String>) -> String {
        let mut out = String::new();
        let mut emit = |key: &str, open: &str, close: &str| {
            if let Some(val) = prop(key) {
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
}
