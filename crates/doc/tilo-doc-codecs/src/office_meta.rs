//! Document-property parts shared by the office codecs: modeled elements are
//! read into `meta.props`, and on export the original part's unmodeled
//! children are carried over verbatim.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeSet;
use tilo_doc::model::DocMut;

/// The element local-name (after any `prefix:`).
pub(crate) fn local_name(name: &[u8]) -> &[u8] {
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

/// Strip elements whose local-name is in `modeled_names`, returning the surviving inner XML.
pub(crate) fn unmodeled_inner(xml: &[u8], container: &str, modeled_names: &[&[u8]]) -> String {
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
