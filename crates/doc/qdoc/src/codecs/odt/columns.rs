//! Multi-column section layout ⇄ ODF page-layout. ODF models newspaper-style
//! columns as a `style:columns` element (carrying `style:column-count` and an
//! optional `style:column-gap`) inside the page-layout's
//! `style:page-layout-properties`, which lives in `styles.xml` under
//! `office:automatic-styles` → `style:page-layout`. This maps that count/gap onto
//! the document-wide column setting (both directions), splicing the regenerated
//! `style:columns` back into the first page-layout while preserving the rest of the
//! page-layout markup.

use crate::model::{columns as model_columns, DocMut, DocRef};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

use super::styles::parse_margin_px;

/// Import the body column count + gap from the first `style:page-layout`'s
/// `style:columns` in a `styles.xml` into the document-wide column setting. No-op
/// when the page-layout carries no `style:columns` (or a count of one) — the
/// document then keeps its single-column default.
pub(super) fn import_page_layout_columns(styles_xml: &[u8], doc: &mut DocMut) {
    let Some((count, gap)) = parse_columns(styles_xml) else { return };
    if count <= 1 {
        return;
    }
    doc.set_columns(count, gap);
}

/// Scan `styles.xml` for the first `style:columns` element and read its
/// `style:column-count` / `style:column-gap`. Returns `None` when no `style:columns`
/// is present.
fn parse_columns(styles_xml: &[u8]) -> Option<(u32, Option<f64>)> {
    let s = std::str::from_utf8(styles_xml).ok()?;
    let mut reader = Reader::from_str(s);
    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => return None,
        };
        match ev {
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"style:columns" => {
                let count = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"style:column-count")
                    .and_then(|a| a.unescape_value().ok().and_then(|v| v.trim().parse::<u32>().ok()))
                    .unwrap_or(1);
                let gap = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"style:column-gap")
                    .and_then(|a| a.unescape_value().ok().and_then(|v| parse_margin_px(&v)));
                return Some((count, gap));
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// px → an ODF length string (`"…cm"`). ODF column gaps are conventionally
/// expressed in cm; px is the model's unit, so convert (96 px/in, 2.54 cm/in).
fn px_to_cm(px: f64) -> String {
    let cm = px * 2.54 / 96.0;
    // Trim to a tidy fixed precision, dropping trailing zeros.
    let mut s = format!("{cm:.4}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    format!("{s}cm")
}

/// The `style:columns` element XML for a `count`/`gap`, or `None` for a single
/// column (a single-column body needs no `style:columns`).
fn columns_xml(count: u32, gap: Option<f64>) -> Option<String> {
    if count <= 1 {
        return None;
    }
    let gap_attr = gap.map(|g| format!(r#" style:column-gap="{}""#, px_to_cm(g))).unwrap_or_default();
    Some(format!(r#"<style:columns style:column-count="{count}"{gap_attr}/>"#))
}

/// Splice the document's column setting into the first `style:page-layout` of
/// `styles_xml`, returning the updated bytes — or `None` when the document is
/// single-column (so the original `styles.xml` is left untouched). The existing
/// `style:page-layout-properties` content (page size, margins, the header/footer
/// the `headers` module wrote, …) is preserved; only the `style:columns` child is
/// replaced. When the source has no `style:page-layout`, one is synthesised so a
/// freshly-authored multi-column document still exports its columns.
pub(super) fn export_page_layout_columns(doc: &DocRef, styles_xml: &[u8]) -> Option<Vec<u8>> {
    let (count, gap) = model_columns(doc.doc, doc.c);
    let cols = columns_xml(count, gap)?; // None ⇒ single column ⇒ leave styles.xml alone
    Some(splice_into_page_layout(styles_xml, &cols))
}

/// Splice `cols` (a `style:columns` element) into the first `style:page-layout`'s
/// `style:page-layout-properties`, replacing any existing `style:columns`. Falls
/// back to synthesising the page-layout / properties element when the source lacks
/// them.
fn splice_into_page_layout(styles_xml: &[u8], cols: &str) -> Vec<u8> {
    let s = String::from_utf8_lossy(styles_xml).into_owned();

    // Case 1: a `style:page-layout-properties` element already exists — drop any
    // existing `style:columns` from it and inject the new one at its end.
    if let Some(props) = find_element_span(&s, "style:page-layout-properties") {
        return splice_into_props(&s, props, cols).into_bytes();
    }

    // Case 2: a `style:page-layout` exists but with no properties element — inject a
    // fresh properties element (carrying just the columns) at its start.
    if let Some(pl) = find_open_tag_end(&s, "<style:page-layout") {
        let props = format!("<style:page-layout-properties>{cols}</style:page-layout-properties>");
        let mut out = String::with_capacity(s.len() + props.len());
        out.push_str(&s[..pl]);
        out.push_str(&props);
        out.push_str(&s[pl..]);
        return out.into_bytes();
    }

    // Case 3: no page-layout at all — synthesise an automatic-styles section with a
    // `pm1` page-layout (matching the name the headers master-page references).
    let section = format!(
        r#"<office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties>{cols}</style:page-layout-properties></style:page-layout></office:automatic-styles>"#
    );
    inject_section(&s, &section).into_bytes()
}

/// Replace/insert `cols` inside a known `style:page-layout-properties` span. The
/// span is `(open_tag_end, inner_start, inner_end)` from [`find_element_span`]; a
/// self-closing properties element has `inner_start == inner_end == open_tag_end`.
fn splice_into_props(s: &str, span: ElementSpan, cols: &str) -> String {
    let ElementSpan { open_start, open_end, inner, self_closing } = span;
    match inner {
        // Self-closing `<style:page-layout-properties .../>` — expand to a paired
        // element holding the columns, preserving the original attributes.
        None => {
            debug_assert!(self_closing);
            // `open_start..open_end` covers `<...props.../>`; rewrite the trailing
            // `/>` into `>{cols}</...>`.
            let tag = &s[open_start..open_end];
            let body = tag.strip_suffix("/>").unwrap_or(tag);
            let mut out = String::with_capacity(s.len() + cols.len() + 64);
            out.push_str(&s[..open_start]);
            out.push_str(body);
            out.push('>');
            out.push_str(cols);
            out.push_str("</style:page-layout-properties>");
            out.push_str(&s[open_end..]);
            out
        }
        Some((inner_start, inner_end)) => {
            let cleaned = strip_columns(&s[inner_start..inner_end]);
            let mut out = String::with_capacity(s.len() + cols.len());
            out.push_str(&s[..inner_start]);
            out.push_str(&cleaned);
            out.push_str(cols); // columns last, after any surviving content
            out.push_str(&s[inner_end..]);
            out
        }
    }
}

struct ElementSpan {
    /// Byte index of the element's opening `<`.
    open_start: usize,
    /// Byte index just past the opening tag's `>`.
    open_end: usize,
    /// `(inner_start, inner_end)` for a paired element, or `None` if self-closing.
    inner: Option<(usize, usize)>,
    self_closing: bool,
}

/// Locate the first `<{name} …>`(…)`</{name}>` (or self-closing `<{name} …/>`)
/// element in `s`.
fn find_element_span(s: &str, name: &str) -> Option<ElementSpan> {
    let open_start = find_tag_start(s, name)?;
    let gt = s[open_start..].find('>')? + open_start;
    let open_end = gt + 1;
    let self_closing = s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/');
    if self_closing {
        return Some(ElementSpan { open_start, open_end, inner: None, self_closing: true });
    }
    let close_tag = format!("</{name}>");
    let inner_end = s[open_end..].find(&close_tag)? + open_end;
    Some(ElementSpan { open_start, open_end, inner: Some((open_end, inner_end)), self_closing: false })
}

/// Byte index of the start (`<`) of the first `<{name}` element whose name matches
/// exactly (the next char after the name is a delimiter, not a name char).
fn find_tag_start(s: &str, name: &str) -> Option<usize> {
    let needle = format!("<{name}");
    let mut from = 0;
    while let Some(rel) = s[from..].find(&needle) {
        let at = from + rel;
        let after = at + needle.len();
        match s.as_bytes().get(after) {
            Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'>') | Some(b'/') => return Some(at),
            _ => from = after,
        }
    }
    None
}

/// Byte index just past the `>` of the first `{open_prefix}` open tag (used to
/// inject content at the element's start).
fn find_open_tag_end(s: &str, open_prefix: &str) -> Option<usize> {
    let open = s.find(open_prefix)?;
    let gt = s[open..].find('>')? + open;
    // A self-closing page-layout can't hold a properties child.
    if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
        return None;
    }
    Some(gt + 1)
}

/// Drop any `style:columns` element (paired or self-closing) from a properties
/// element's inner XML, returning the survivors.
fn strip_columns(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut reader = Reader::from_str(inner);
    let mut skip_depth = 0usize;
    loop {
        let pos = reader.buffer_position() as usize;
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        let after = reader.buffer_position() as usize;
        match &ev {
            Event::Start(e) => {
                if skip_depth > 0 {
                    skip_depth += 1;
                } else if e.name().as_ref() == b"style:columns" {
                    skip_depth = 1;
                } else {
                    out.push_str(&inner[pos..after]);
                }
            }
            Event::Empty(e) => {
                if skip_depth == 0 && e.name().as_ref() != b"style:columns" {
                    out.push_str(&inner[pos..after]);
                }
            }
            Event::End(_) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                } else {
                    out.push_str(&inner[pos..after]);
                }
            }
            Event::Eof => break,
            _ => {
                if skip_depth == 0 {
                    out.push_str(&inner[pos..after]);
                }
            }
        }
    }
    out
}

/// Inject a fresh `office:automatic-styles` `section` into a `styles.xml` with no
/// page-layout. Prefer placing it before `office:master-styles` (ODF orders
/// automatic-styles before master-styles); otherwise before the document-styles
/// close; otherwise append.
fn inject_section(s: &str, section: &str) -> String {
    let mut at = s.find("<office:master-styles");
    if at.is_none() {
        at = s.rfind("</office:document-styles>");
    }
    match at {
        Some(pos) => {
            let mut out = String::with_capacity(s.len() + section.len());
            out.push_str(&s[..pos]);
            out.push_str(section);
            out.push_str(&s[pos..]);
            out
        }
        None => {
            let mut out = s.to_string();
            out.push_str(section);
            out
        }
    }
}
