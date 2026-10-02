//! Multi-column section layout ⇄ ODF page-layout. ODF models newspaper-style columns as a
//! `style:columns` element (`style:column-count` + optional `style:column-gap`) inside the
//! page-layout's `style:page-layout-properties`, under `office:automatic-styles >
//! style:page-layout`. This maps that count/gap onto the document-wide column setting
//! (both directions), splicing `style:columns` back into the first page-layout in place.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tilo_doc::model::{DocMut, DocRef, columns as model_columns};

use super::styles::parse_margin_px;

/// Import the body column count + gap from the first `style:page-layout`'s `style:columns`
/// into the document-wide setting. No-op with no `style:columns` (or a count of one).
pub(super) fn import_page_layout_columns(styles_xml: &[u8], doc: &mut DocMut) {
    let Some((count, gap)) = parse_columns(styles_xml) else {
        return;
    };
    if count <= 1 {
        return;
    }
    doc.set_columns(count, gap);
}

/// Scan `styles.xml` for the first `style:columns` element and read its count/gap.
fn parse_columns(styles_xml: &[u8]) -> Option<(u32, Option<f64>)> {
    let s = std::str::from_utf8(styles_xml).ok()?;
    let mut reader = Reader::from_str(s);
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) | Event::Empty(e) if e.name().as_ref() == b"style:columns" => {
                let count = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"style:column-count")
                    .and_then(|a| crate::unescape_attr(&a))
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(1);
                let gap = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"style:column-gap")
                    .and_then(|a| crate::unescape_attr(&a))
                    .and_then(|v| parse_margin_px(&v));
                return Some((count, gap));
            }
            Event::Eof => return None,
            _ => {}
        }
    }
    None
}

/// px → an ODF length string (`"…cm"`); ODF column gaps are conventionally cm, while px is the model's unit (96 px/in, 2.54 cm/in).
fn px_to_cm(px: f64) -> String {
    let cm = px * 2.54 / 96.0;
    let mut s = format!("{cm:.4}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    format!("{s}cm")
}

/// The `style:columns` element XML for a `count`/`gap`, or `None` for a single column.
fn columns_xml(count: u32, gap: Option<f64>) -> Option<String> {
    if count <= 1 {
        return None;
    }
    let gap_attr = gap
        .map(|g| format!(r#" style:column-gap="{}""#, px_to_cm(g)))
        .unwrap_or_default();
    Some(format!(
        r#"<style:columns style:column-count="{count}"{gap_attr}/>"#
    ))
}

/// Splice the document's column setting into the first `style:page-layout`, or `None`
/// when single-column (original left untouched). Existing `style:page-layout-properties`
/// content (page size, margins, header/footer) is preserved — only `style:columns` is
/// replaced; a page-layout is synthesised if the source has none.
pub(super) fn export_page_layout_columns(doc: &DocRef, styles_xml: &[u8]) -> Option<Vec<u8>> {
    let (count, gap) = model_columns(doc.doc, doc.c);
    let cols = columns_xml(count, gap)?; // None ⇒ single column ⇒ leave styles.xml alone
    Some(splice_into_page_layout(styles_xml, &cols))
}

/// Splice `cols` into the first `style:page-layout`'s `style:page-layout-properties`,
/// replacing any existing `style:columns`, synthesising the page-layout/properties if absent.
fn splice_into_page_layout(styles_xml: &[u8], cols: &str) -> Vec<u8> {
    let s = String::from_utf8_lossy(styles_xml).into_owned();

    // Case 1: a `style:page-layout-properties` element already exists — drop any existing `style:columns` and inject the new one at its end.
    if let Some(props) = find_element_span(&s, "style:page-layout-properties") {
        return splice_into_props(&s, &props, cols).into_bytes();
    }

    // Case 2: a `style:page-layout` exists with no properties element — inject a fresh one (carrying just the columns) at its start.
    if let Some(pl) = find_open_tag_end(&s, "<style:page-layout") {
        let props = format!("<style:page-layout-properties>{cols}</style:page-layout-properties>");
        if let (Some(head), Some(tail)) = (s.get(..pl), s.get(pl..)) {
            let mut out = String::with_capacity(s.len().saturating_add(props.len()));
            out.push_str(head);
            out.push_str(&props);
            out.push_str(tail);
            return out.into_bytes();
        }
        return s.into_bytes();
    }

    // Case 3: no page-layout at all — synthesise an automatic-styles section with a `pm1`
    // page-layout (matching the name the headers master-page references).
    let section = format!(
        r#"<office:automatic-styles><style:page-layout style:name="pm1"><style:page-layout-properties>{cols}</style:page-layout-properties></style:page-layout></office:automatic-styles>"#
    );
    inject_section(&s, &section).into_bytes()
}

/// Replace/insert `cols` inside a known `style:page-layout-properties` span; a self-closing
/// properties element has `inner_start == inner_end == open_tag_end`.
fn splice_into_props(s: &str, span: &ElementSpan, cols: &str) -> String {
    let ElementSpan {
        open_start,
        open_end,
        inner,
        self_closing,
    } = *span;
    match inner {
        // Self-closing — expand to a paired element holding the columns, preserving the original attributes.
        None => {
            debug_assert!(self_closing);
            // `open_start..open_end` covers `<...props.../>`; rewrite the trailing `/>` into `>{cols}</...>`.
            let tag = s.get(open_start..open_end).unwrap_or_default();
            let body = tag.strip_suffix("/>").unwrap_or(tag);
            let mut out =
                String::with_capacity(s.len().saturating_add(cols.len()).saturating_add(64));
            out.push_str(s.get(..open_start).unwrap_or_default());
            out.push_str(body);
            out.push('>');
            out.push_str(cols);
            out.push_str("</style:page-layout-properties>");
            out.push_str(s.get(open_end..).unwrap_or_default());
            out
        }
        Some((inner_start, inner_end)) => {
            let cleaned = s
                .get(inner_start..inner_end)
                .map(strip_columns)
                .unwrap_or_default();
            let mut out = String::with_capacity(s.len().saturating_add(cols.len()));
            out.push_str(s.get(..inner_start).unwrap_or_default());
            out.push_str(&cleaned);
            out.push_str(cols); // columns last, after any surviving content
            out.push_str(s.get(inner_end..).unwrap_or_default());
            out
        }
    }
}

#[derive(Clone, Copy)]
struct ElementSpan {
    /// Byte index of the element's opening `<`.
    open_start: usize,
    /// Byte index just past the opening tag's `>`.
    open_end: usize,
    /// `(inner_start, inner_end)` for a paired element, or `None` if self-closing.
    inner: Option<(usize, usize)>,
    self_closing: bool,
}

/// Locate the first `<{name} …>`(…)`</{name}>` (or self-closing) element in `s`.
fn find_element_span(s: &str, name: &str) -> Option<ElementSpan> {
    let open_start = find_tag_start(s, name)?;
    let gt = s.get(open_start..)?.find('>')?.checked_add(open_start)?;
    let open_end = gt.checked_add(1)?;
    let self_closing = s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/');
    if self_closing {
        return Some(ElementSpan {
            open_start,
            open_end,
            inner: None,
            self_closing: true,
        });
    }
    let close_tag = format!("</{name}>");
    let inner_end = s.get(open_end..)?.find(&close_tag)?.checked_add(open_end)?;
    Some(ElementSpan {
        open_start,
        open_end,
        inner: Some((open_end, inner_end)),
        self_closing: false,
    })
}

/// Byte index of the start of the first `<{name}` element whose name matches exactly.
fn find_tag_start(s: &str, name: &str) -> Option<usize> {
    let needle = format!("<{name}");
    let mut from = 0;
    while let Some(rel) = s.get(from..).and_then(|rest| rest.find(&needle)) {
        let at = from.checked_add(rel)?;
        let after = at.checked_add(needle.len())?;
        match s.as_bytes().get(after) {
            Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/') => {
                return Some(at);
            }
            _ => from = after,
        }
    }
    None
}

/// Byte index just past the `>` of the first `{open_prefix}` open tag.
fn find_open_tag_end(s: &str, open_prefix: &str) -> Option<usize> {
    let open = s.find(open_prefix)?;
    let gt = s.get(open..)?.find('>')?.checked_add(open)?;
    if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
        return None;
    }
    gt.checked_add(1)
}

/// Drop any `style:columns` element (paired or self-closing) from a properties element's inner XML.
fn strip_columns(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut reader = Reader::from_str(inner);
    let mut skip_depth = 0usize;
    while let Ok(pos) = usize::try_from(reader.buffer_position()) {
        let Ok(ev) = reader.read_event() else {
            break;
        };
        let Ok(after) = usize::try_from(reader.buffer_position()) else {
            break;
        };
        let chunk = inner.get(pos..after).unwrap_or_default();
        match &ev {
            Event::Start(e) => {
                if skip_depth > 0 {
                    skip_depth = skip_depth.saturating_add(1);
                } else if e.name().as_ref() == b"style:columns" {
                    skip_depth = 1;
                } else {
                    out.push_str(chunk);
                }
            }
            Event::Empty(e) => {
                if skip_depth == 0 && e.name().as_ref() != b"style:columns" {
                    out.push_str(chunk);
                }
            }
            Event::End(_) => {
                if skip_depth > 0 {
                    skip_depth = skip_depth.saturating_sub(1);
                } else {
                    out.push_str(chunk);
                }
            }
            Event::Eof => break,
            _ => {
                if skip_depth == 0 {
                    out.push_str(chunk);
                }
            }
        }
    }
    out
}

/// Inject a fresh `office:automatic-styles` `section` into a `styles.xml` with no page-layout.
/// ODF orders automatic-styles before master-styles, so prefer before `office:master-styles`;
/// otherwise before the document-styles close; otherwise append.
fn inject_section(s: &str, section: &str) -> String {
    let mut at = s.find("<office:master-styles");
    if at.is_none() {
        at = s.rfind("</office:document-styles>");
    }
    if let Some(pos) = at
        && let (Some(head), Some(tail)) = (s.get(..pos), s.get(pos..))
    {
        let mut out = String::with_capacity(s.len().saturating_add(section.len()));
        out.push_str(head);
        out.push_str(section);
        out.push_str(tail);
        return out;
    }
    let mut out = s.to_string();
    out.push_str(section);
    out
}
