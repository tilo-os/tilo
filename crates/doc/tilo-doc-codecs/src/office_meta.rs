//! Shared by the document codecs' metadata parts (`docProps/*.xml`, `meta.xml`): both are a
//! flat list of elements, read into `meta.props` by element local-name, and written back by
//! regenerating the modeled elements while copying every other element through verbatim.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeSet;
use tilo_doc::model::DocMut;

/// The element local-name (after any `prefix:`).
fn local_name(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => name.get(i.saturating_add(1)..).unwrap_or(name),
        None => name,
    }
}

/// Collect the text content of every element matching `prop_of` into `meta.props`.
pub(crate) fn import_elements(
    xml: &[u8],
    doc: &mut DocMut,
    prop_of: impl Fn(&[u8]) -> Option<&'static str>,
) {
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

/// Strip the elements whose local-name is in `modeled_names` from a container's inner XML,
/// keeping the unmodeled ones verbatim so they survive the round-trip.
pub(crate) fn strip_modeled(inner: &[u8], modeled_names: &[&[u8]]) -> String {
    let modeled: BTreeSet<&[u8]> = modeled_names.iter().copied().collect();
    let mut reader = Reader::from_reader(inner);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out: Vec<u8> = Vec::new();
    // Depth of the modeled element we're currently skipping (0 = not skipping).
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
