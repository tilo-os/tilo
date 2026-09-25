//! Paragraph properties (`w:pPr`) the model does not represent — indentation, keep-with-next,
//! page break before, borders, shading, tabs, the paragraph mark's run properties, a section
//! break's `w:sectPr`, … — kept verbatim on the block (attr [`ATTR_PPR`]) and merged back with
//! the modeled properties (style, numbering, alignment, line spacing) on export, in the
//! element order `CT_PPr` requires.

use super::styles::line_spacing_from_spacing;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Block attr holding the unmodeled `w:pPr` children of an imported paragraph, verbatim.
pub(super) const ATTR_PPR: &str = "docx_ppr";

/// `CT_PPr`'s child order. Word rejects a `w:pPr` whose children are out of order.
const PPR_ORDER: &[&[u8]] = &[
    b"w:pStyle",
    b"w:keepNext",
    b"w:keepLines",
    b"w:pageBreakBefore",
    b"w:framePr",
    b"w:widowControl",
    b"w:numPr",
    b"w:suppressLineNumbers",
    b"w:pBdr",
    b"w:shd",
    b"w:tabs",
    b"w:suppressAutoHyphens",
    b"w:kinsoku",
    b"w:wordWrap",
    b"w:overflowPunct",
    b"w:topLinePunct",
    b"w:autoSpaceDE",
    b"w:autoSpaceDN",
    b"w:bidi",
    b"w:adjustRightInd",
    b"w:snapToGrid",
    b"w:spacing",
    b"w:ind",
    b"w:contextualSpacing",
    b"w:mirrorIndents",
    b"w:suppressOverlap",
    b"w:jc",
    b"w:textDirection",
    b"w:textAlignment",
    b"w:textboxTightWrap",
    b"w:outlineLvl",
    b"w:divId",
    b"w:cnfStyle",
    b"w:rPr",
    b"w:sectPr",
    b"w:pPrChange",
];

/// Where `name` goes in a `w:pPr`; an element `CT_PPr` does not list (an extension) goes just
/// before the paragraph mark's run properties.
fn order_of(name: &[u8]) -> usize {
    PPR_ORDER
        .iter()
        .position(|n| *n == name)
        .or_else(|| PPR_ORDER.iter().position(|n| *n == b"w:rPr"))
        .unwrap_or(0)
}

/// What of an imported `w:pPr` child to keep verbatim: `None` for what the model represents
/// (numbering, alignment); the `w:spacing` without the line spacing the model holds.
pub(super) fn kept_child(e: &BytesStart, raw: &str) -> Option<String> {
    match e.name().as_ref() {
        b"w:numPr" | b"w:jc" => None,
        b"w:spacing" if line_spacing_from_spacing(e).is_some() => {
            let mut out = String::from("<w:spacing");
            for a in e.attributes().flatten() {
                if matches!(a.key.as_ref(), b"w:line" | b"w:lineRule") {
                    continue;
                }
                out.push(' ');
                out.push_str(&String::from_utf8_lossy(a.key.as_ref()));
                out.push_str("=\"");
                out.push_str(&String::from_utf8_lossy(&a.value));
                out.push('"');
            }
            out.push_str("/>");
            (out != "<w:spacing/>").then_some(out)
        }
        _ => Some(raw.to_string()),
    }
}

/// The sibling elements of an XML fragment as `(name, verbatim XML)`.
fn children(xml: &str) -> Vec<(Vec<u8>, String)> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut open: Option<(usize, Vec<u8>)> = None;
    loop {
        let before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else {
            return out;
        };
        let after = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match ev {
            Event::Start(e) => {
                if depth == 0 {
                    open = Some((before, e.name().as_ref().to_vec()));
                }
                depth = depth.saturating_add(1);
            }
            Event::Empty(e) if depth == 0 => out.push((
                e.name().as_ref().to_vec(),
                xml.get(before..after).unwrap_or_default().to_string(),
            )),
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some((start, name)) = open.take()
                {
                    out.push((name, xml.get(start..after).unwrap_or_default().to_string()));
                }
            }
            Event::Eof => return out,
            _ => {}
        }
    }
}

/// The section break (`w:sectPr`) among a paragraph's kept properties, if any.
pub(super) fn section_break(kept: &str) -> Option<String> {
    children(kept)
        .into_iter()
        .find(|(name, _)| name == b"w:sectPr")
        .map(|(_, raw)| raw)
}

/// Whether a paragraph's kept properties hold a style reference of their own.
pub(super) fn has_style(kept: &str) -> bool {
    children(kept).iter().any(|(name, _)| name == b"w:pStyle")
}

/// The attributes of an empty element's XML (`<w:x a="1" b="2"/>` → ` a="1" b="2"`).
fn empty_element_attrs(xml: &str) -> &str {
    let body = xml.trim().trim_end_matches("/>").trim_end_matches('>');
    body.find(char::is_whitespace)
        .and_then(|at| body.get(at..))
        .unwrap_or_default()
}

/// A complete `w:pPr` (or nothing, when empty) from the `modeled` children the exporter
/// writes and the `kept` ones carried from import. A modeled child replaces a kept one of the
/// same name, except `w:spacing`, whose kept attributes (before/after) join the modeled line
/// spacing. `section` rewrites a kept section break's `w:sectPr` for this export.
pub(super) fn ppr_xml(
    modeled: &[(&[u8], String)],
    kept: Option<&str>,
    section: impl Fn(&str) -> String,
) -> String {
    let mut all: Vec<(Vec<u8>, String)> = modeled
        .iter()
        .filter(|(_, xml)| !xml.is_empty())
        .map(|(name, xml)| (name.to_vec(), xml.clone()))
        .collect();
    for (name, raw) in kept.map(children).unwrap_or_default() {
        if let Some((_, xml)) = all.iter_mut().find(|(n, _)| *n == name) {
            if name == b"w:spacing" {
                *xml = format!(
                    "<w:spacing{}{}/>",
                    empty_element_attrs(&raw),
                    empty_element_attrs(xml)
                );
            }
            continue;
        }
        let raw = if name == b"w:sectPr" {
            section(&raw)
        } else {
            raw
        };
        all.push((name, raw));
    }
    if all.is_empty() {
        return String::new();
    }
    all.sort_by_key(|(name, _)| order_of(name));
    let inner: String = all.into_iter().map(|(_, xml)| xml).collect();
    format!("<w:pPr>{inner}</w:pPr>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_kept_and_modeled_children_in_schema_order() {
        let kept = r#"<w:pStyle w:val="Normal"/><w:ind w:start="720"/><w:spacing w:before="120"/><w:sectPr><w:cols w:num="2"/></w:sectPr>"#;
        let modeled: Vec<(&[u8], String)> = vec![
            (b"w:jc", r#"<w:jc w:val="center"/>"#.to_string()),
            (
                b"w:spacing",
                r#"<w:spacing w:line="360" w:lineRule="auto"/>"#.to_string(),
            ),
            (b"w:pStyle", String::new()),
        ];
        let out = ppr_xml(&modeled, Some(kept), |s| {
            s.replace("w:num=\"2\"", "w:num=\"3\"")
        });
        assert_eq!(
            out,
            concat!(
                r#"<w:pPr><w:pStyle w:val="Normal"/>"#,
                r#"<w:spacing w:before="120" w:line="360" w:lineRule="auto"/>"#,
                r#"<w:ind w:start="720"/><w:jc w:val="center"/>"#,
                r#"<w:sectPr><w:cols w:num="3"/></w:sectPr></w:pPr>"#,
            )
        );
    }
}
