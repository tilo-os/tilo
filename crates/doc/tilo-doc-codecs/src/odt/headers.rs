//! Page header/footer ⇄ ODF master-page. ODF keeps repeated page regions in `styles.xml`
//! under `office:master-styles > style:master-page > style:header`/`style:footer`
//! (a page-number field is `<text:page-number/>`). This maps them onto the canonical
//! header/footer regions, splicing regenerated regions back into a copy of `styles.xml`.

use super::plain_paragraphs;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use tilo_doc::model::{DocMut, DocRef, Region, materialize_region};
use tilo_doc::schema::{FIELD_PAGE_NUMBER, FIELD_PLACEHOLDER, MARK_FIELD};
use tilo_doc::snapshot::BlockView;
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
    // Pending field marks (block, start) applied after text is in place.
    let mut field_marks: Vec<(String, usize)> = Vec::new();
    plain_paragraphs::import(
        xml,
        Some(tag.as_bytes()),
        doc,
        |doc, prev| Some(doc.insert_block_in(region, prev, "paragraph")),
        |doc, block, at, text| doc.splice_text_in(region, block, at, 0, text),
        |name, block, at| {
            (name == b"text:page-number").then(|| {
                field_marks.push((block.to_owned(), at));
                FIELD_PLACEHOLDER.to_string()
            })
        },
    );

    for (b, start) in field_marks {
        doc.add_mark_in(
            region,
            &b,
            start,
            start.saturating_add(1),
            MARK_FIELD,
            FIELD_PAGE_NUMBER,
        );
    }
}

/// Whether character `i` of `b` is the start of a page-number field.
fn page_number_field_at(b: &BlockView, i: usize) -> bool {
    b.marks.iter().any(|m| {
        m.name == MARK_FIELD
            && m.start == i
            && m.end == i.saturating_add(1)
            && m.value.as_str() == Some(FIELD_PAGE_NUMBER)
    })
}

/// Render a region block, emitting `<text:page-number/>` for page-number fields.
fn render_region_block(b: &BlockView) -> String {
    let chars: Vec<char> = b.text.chars().collect();
    plain_paragraphs::render_one(b, |i| {
        (chars.get(i) == Some(&FIELD_PLACEHOLDER)).then(|| {
            if page_number_field_at(b, i) {
                "<text:page-number/>"
            } else {
                ""
            }
        })
    })
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
///
/// A region the model holds unedited (it equals a fresh import of the original) keeps its
/// original elements, left/first-page variants and everything the model does not hold
/// included; when neither region was edited `styles.xml` stays as it is.
pub(super) fn export_master_headers(doc: &DocRef, original: Option<&[u8]>) -> Option<Vec<u8>> {
    let header = materialize_region(doc.doc, doc.c, Region::Header);
    let footer = materialize_region(doc.doc, doc.c, Region::Footer);
    let kept = original.map(|bytes| {
        let s = String::from_utf8_lossy(bytes).into_owned();
        let mut scratch = tilo_doc::Document::new();
        import_master_headers(bytes, &mut scratch.as_mut());
        let fresh = scratch.as_ref();
        let unchanged = |region: Region, now: &[BlockView]| {
            crate::preserve::same_blocks(now, &materialize_region(fresh.doc, fresh.c, region))
        };
        let inner = first_master_page_span(&s)
            .and_then(|(_, start, end)| s.get(start..end).map(str::to_string))
            .unwrap_or_default();
        (
            unchanged(Region::Header, &header).then(|| region_elements(&inner, true)),
            unchanged(Region::Footer, &footer).then(|| region_elements(&inner, false)),
        )
    });
    let (kept_header, kept_footer) = kept.unwrap_or_default();
    if kept_header.is_some() && kept_footer.is_some() {
        return None;
    }
    let header_xml = kept_header.or_else(|| region_xml(&header, "style:header"));
    let footer_xml = kept_footer.or_else(|| region_xml(&footer, "style:footer"));
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
        let inner = s.get(mp_inner_start..mp_inner_end).unwrap_or_default();
        let cleaned = strip_regions(inner);
        let mut out = String::with_capacity(s.len().saturating_add(regions.len()));
        out.push_str(s.get(..open_end).unwrap_or_default());
        out.push_str(regions);
        out.push_str(&cleaned);
        out.push_str(s.get(mp_inner_end..).unwrap_or_default());
        return out.into_bytes();
    }
    // No master-page: inject a fresh master-styles section, preferring to place it before the document-styles close.
    let section = format!(
        r#"<office:master-styles><style:master-page style:name="Standard">{regions}</style:master-page></office:master-styles>"#
    );
    if let Some(pos) = s.rfind("</office:document-styles>") {
        let mut out = String::with_capacity(s.len().saturating_add(section.len()));
        out.push_str(s.get(..pos).unwrap_or_default());
        out.push_str(&section);
        out.push_str(s.get(pos..).unwrap_or_default());
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
    let gt = open.checked_add(s.get(open..)?.find('>')?)?;
    if s.as_bytes().get(gt.wrapping_sub(1)) == Some(&b'/') {
        return None;
    }
    let inner_start = gt.checked_add(1)?;
    let close = inner_start.checked_add(s.get(inner_start..)?.find("</style:master-page>")?)?;
    Some((inner_start, inner_start, close))
}

/// Remove `style:header`/`style:footer` (and the rarely-used `*-left`/`*-first` variants) from a master-page's inner XML.
fn strip_regions(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut reader = Reader::from_str(inner);
    let mut skip_depth = 0usize;
    loop {
        let pos = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else {
            break;
        };
        let after = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let span = inner.get(pos..after).unwrap_or_default();
        match &ev {
            Event::Start(e) => {
                if skip_depth > 0 {
                    skip_depth = skip_depth.saturating_add(1);
                } else if is_region_tag(e.name().as_ref()) {
                    skip_depth = 1;
                } else {
                    out.push_str(span);
                }
            }
            Event::Empty(e) => {
                if skip_depth == 0 && !is_region_tag(e.name().as_ref()) {
                    out.push_str(span);
                }
            }
            Event::End(_) => {
                if skip_depth > 0 {
                    skip_depth = skip_depth.saturating_sub(1);
                } else {
                    out.push_str(span);
                }
            }
            Event::Eof => break,
            _ => {
                if skip_depth == 0 {
                    out.push_str(span);
                }
            }
        }
    }
    out
}

/// The verbatim header (`headers`) or footer elements of a master-page's inner XML, the
/// left/first-page variants included.
fn region_elements(inner: &str, headers: bool) -> String {
    let wanted = |name: &[u8]| {
        is_region_tag(name)
            && name.starts_with(if headers {
                b"style:header"
            } else {
                b"style:footer"
            })
    };
    let mut out = String::new();
    let mut reader = Reader::from_str(inner);
    let mut open: Option<(usize, usize)> = None; // (start, depth)
    loop {
        let pos = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else {
            break;
        };
        let after = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match (&ev, open.as_mut()) {
            (Event::Start(_), Some((_, depth))) => *depth = depth.saturating_add(1),
            (Event::End(_), Some((start, depth))) => {
                if *depth == 0 {
                    out.push_str(inner.get(*start..after).unwrap_or_default());
                    open = None;
                } else {
                    *depth = depth.saturating_sub(1);
                }
            }
            (Event::Start(e), None) if wanted(e.name().as_ref()) => open = Some((pos, 0)),
            (Event::Empty(e), None) if wanted(e.name().as_ref()) => {
                out.push_str(inner.get(pos..after).unwrap_or_default());
            }
            (Event::Eof, _) => break,
            _ => {}
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
