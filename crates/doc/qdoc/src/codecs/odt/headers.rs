//! Page header/footer ⇄ ODF master-page. ODF keeps repeated page regions in `styles.xml`
//! under `office:master-styles > style:master-page > style:header`/`style:footer`
//! (a page-number field is `<text:page-number/>`). This maps them onto the canonical
//! header/footer regions, splicing regenerated regions back into a copy of `styles.xml`.

use crate::model::{DocMut, DocRef, Region, materialize_region};
use crate::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_FIELD};
use crate::snapshot::BlockView;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
// `write!` into a `String` cannot fail; the discarded `Result` is the
// `fmt::Write` contract, not a swallowed error.
use std::fmt::Write as _;

/// Import the `style:header`/`style:footer` paragraphs from the first `style:master-page`.
/// `<text:page-number/>` becomes a page-number field. No-op when neither region is present.
pub(super) fn import_master_headers(styles_xml: &[u8], doc: &mut DocMut) {
    let Ok(s) = std::str::from_utf8(styles_xml) else {
        return;
    };
    parse_region(s, "style:header", Region::Header, doc);
    parse_region(s, "style:footer", Region::Footer, doc);
}

/// Walk a `style:header`/`style:footer` element into region blocks; text,
/// `text:tab`/`text:line-break`/`text:s`, and `text:page-number` fields map onto the model.
fn parse_region(xml: &str, tag: &str, region: Region, doc: &mut DocMut) {
    let mut reader = Reader::from_str(xml);
    let mut in_region = false;
    let mut prev: Option<String> = None;
    let mut cur: Option<String> = None;
    let mut cur_len: usize = 0;
    // Pending field marks (block, start) applied after text is in place.
    let mut field_marks: Vec<(String, usize)> = Vec::new();

    let open_block = |doc: &mut DocMut,
                      prev: &mut Option<String>,
                      cur: &mut Option<String>,
                      cur_len: &mut usize| {
        let id = doc.insert_block_in(region, prev.as_deref(), "paragraph");
        *prev = Some(id.clone());
        *cur = Some(id);
        *cur_len = 0;
    };
    let append = |doc: &mut DocMut, cur: &Option<String>, cur_len: &mut usize, txt: &str| {
        if let Some(b) = cur
            && !txt.is_empty()
        {
            doc.splice_text_in(region, b, *cur_len, 0, txt);
            *cur_len += txt.chars().count();
        }
    };

    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                n if n == tag.as_bytes() => in_region = true,
                b"text:p" | b"text:h" if in_region => {
                    open_block(doc, &mut prev, &mut cur, &mut cur_len);
                }
                _ => {}
            },
            Event::Empty(e) if in_region => match e.name().as_ref() {
                b"text:p" | b"text:h" => {
                    open_block(doc, &mut prev, &mut cur, &mut cur_len);
                    cur = None;
                }
                b"text:page-number" => {
                    if let Some(b) = cur.clone() {
                        let start = cur_len;
                        append(doc, &cur, &mut cur_len, &FIELD_PLACEHOLDER.to_string());
                        field_marks.push((b, start));
                    }
                }
                b"text:tab" => append(doc, &cur, &mut cur_len, "\t"),
                b"text:line-break" => append(doc, &cur, &mut cur_len, "\n"),
                b"text:s" => {
                    let n = e
                        .attributes()
                        .flatten()
                        .find(|a| a.key.as_ref() == b"text:c")
                        .and_then(|a| {
                            a.unescape_value()
                                .ok()
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap_or(1)
                        .min(256);
                    append(doc, &cur, &mut cur_len, &" ".repeat(n));
                }
                _ => {}
            },
            Event::Text(t) if in_region && cur.is_some() => {
                if let Ok(txt) = t.xml10_content() {
                    // Skip pretty-printer indentation between structural elements.
                    if !txt.is_empty() && !super::content::is_layout_whitespace(&txt) {
                        append(doc, &cur, &mut cur_len, &txt);
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                n if n == tag.as_bytes() => in_region = false,
                b"text:p" | b"text:h" if in_region => cur = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    // Apply page-number fields now that all text is present.
    for (b, start) in field_marks {
        doc.add_mark_in(region, &b, start, start + 1, MARK_FIELD, FIELD_PAGE_NUMBER);
    }
}

fn push_escaped(out: &mut String, c: char) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(c),
    }
}

/// Whether character `i` of `b` is the start of a page-number field.
fn page_number_field_at(b: &BlockView, i: usize) -> bool {
    b.marks.iter().any(|m| {
        m.name == MARK_FIELD
            && m.start == i
            && m.end == i + 1
            && m.value.as_str() == Some(FIELD_PAGE_NUMBER)
    })
}

/// Render a region block's text, emitting `<text:page-number/>` for field placeholders
/// and `text:tab`/`text:line-break` for tabs/breaks; richer inline styling isn't modeled.
fn render_region_block(b: &BlockView) -> String {
    let chars: Vec<char> = b.text.chars().collect();
    let mut out = String::from("<text:p>");
    for (i, &c) in chars.iter().enumerate() {
        if c == FIELD_PLACEHOLDER {
            if page_number_field_at(b, i) {
                out.push_str("<text:page-number/>");
            }
            continue;
        }
        match c {
            '\t' => out.push_str("<text:tab/>"),
            '\n' => out.push_str("<text:line-break/>"),
            _ => push_escaped(&mut out, c),
        }
    }
    out.push_str("</text:p>");
    out
}

/// The `style:header`/`style:footer` XML for a region's blocks, or `None` when empty.
fn region_xml(blocks: &[BlockView], tag: &str) -> Option<String> {
    if blocks.is_empty() {
        return None;
    }
    let mut out = format!("<{tag}>");
    for b in blocks {
        out.push_str(&render_region_block(b));
    }
    let _ = write!(out, "</{tag}>");
    Some(out)
}

/// Inject (or replace) the `style:header`/`style:footer` children of the first
/// `style:master-page`, synthesising a minimal one if none exists; `None` when the
/// document has neither header nor footer, leaving `styles.xml` untouched.
pub(super) fn export_master_headers(doc: &DocRef, original: Option<&[u8]>) -> Option<Vec<u8>> {
    let header = materialize_region(doc.doc, doc.c, Region::Header);
    let footer = materialize_region(doc.doc, doc.c, Region::Footer);
    let header_xml = region_xml(&header, "style:header");
    let footer_xml = region_xml(&footer, "style:footer");
    if header_xml.is_none() && footer_xml.is_none() {
        return None;
    }
    // ODF orders `style:header` before `style:footer` inside a master-page.
    let mut regions = String::new();
    if let Some(h) = &header_xml {
        regions.push_str(h);
    }
    if let Some(f) = &footer_xml {
        regions.push_str(f);
    }

    match original {
        Some(bytes) => Some(splice_into_master(bytes, &regions)),
        None => Some(synth_styles(&regions).into_bytes()),
    }
}

/// Build a fresh minimal `styles.xml` carrying just the master-page header/footer.
fn synth_styles(regions: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"><office:master-styles><style:master-page style:name="Standard" style:page-layout-name="pm1">{regions}</style:master-page></office:master-styles></office:document-styles>"#
    )
}

/// Splice `regions` into the first `style:master-page` of an existing `styles.xml`: strip
/// its existing header/footer, insert the new ones at its start, or append a master-styles section if there's no master-page.
fn splice_into_master(styles_xml: &[u8], regions: &str) -> Vec<u8> {
    let s = String::from_utf8_lossy(styles_xml).into_owned();
    if let Some((open_end, mp_inner_start, mp_inner_end)) = first_master_page_span(&s) {
        // Drop existing header/footer from the master-page's inner content; keep everything else (page-layout refs etc.) verbatim.
        let inner = &s[mp_inner_start..mp_inner_end];
        let cleaned = strip_regions(inner);
        let mut out = String::with_capacity(s.len() + regions.len());
        out.push_str(&s[..open_end]);
        out.push_str(regions);
        out.push_str(&cleaned);
        out.push_str(&s[mp_inner_end..]);
        return out.into_bytes();
    }
    // No master-page: inject a fresh master-styles section, preferring to place it before the document-styles close.
    let section = format!(
        r#"<office:master-styles><style:master-page style:name="Standard">{regions}</style:master-page></office:master-styles>"#
    );
    if let Some(pos) = s.rfind("</office:document-styles>") {
        let mut out = String::with_capacity(s.len() + section.len());
        out.push_str(&s[..pos]);
        out.push_str(&section);
        out.push_str(&s[pos..]);
        out.into_bytes()
    } else {
        let mut out = s;
        out.push_str(&section);
        out.into_bytes()
    }
}

/// `(byte index just past the master-page open tag '>', inner start, inner end)`
/// for the first `style:master-page` element, or `None` if absent / self-closing.
fn first_master_page_span(s: &str) -> Option<(usize, usize, usize)> {
    let open = s.find("<style:master-page")?;
    let gt = s[open..].find('>')? + open;
    if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
        return None;
    }
    let inner_start = gt + 1;
    let close = s[inner_start..].find("</style:master-page>")? + inner_start;
    Some((inner_start, inner_start, close))
}

/// Remove `style:header`/`style:footer` (and the rarely-used `*-left`/`*-first` variants) from a master-page's inner XML.
fn strip_regions(inner: &str) -> String {
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
                } else if is_region_tag(e.name().as_ref()) {
                    skip_depth = 1;
                } else {
                    out.push_str(&inner[pos..after]);
                }
            }
            Event::Empty(e) => {
                if skip_depth == 0 && !is_region_tag(e.name().as_ref()) {
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

fn is_region_tag(name: &[u8]) -> bool {
    matches!(
        name,
        b"style:header"
            | b"style:footer"
            | b"style:header-left"
            | b"style:footer-left"
            | b"style:header-first"
            | b"style:footer-first"
    )
}
