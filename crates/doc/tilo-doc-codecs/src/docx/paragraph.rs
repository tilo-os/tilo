//! Paragraph properties (`w:pPr`) the model does not represent — indentation, keep-with-next,
//! page break before, borders, shading, tabs, the paragraph mark's run properties, a section
//! break's `w:sectPr`, … — kept verbatim on the block (attr [`ATTR_PPR`]) and merged back with
//! the modeled properties (style, numbering, alignment, line spacing) on export, in the
//! element order `CT_PPr` requires.
//!
//! Run properties (`w:rPr`) the model does not represent — the character style, caps,
//! character spacing, kerning, hidden text, the original font names, … — likewise ride on the
//! run's text as a [`MARK_RPR`] mark and are merged back in `CT_RPr` order.

use super::styles::{attr_val, line_spacing_from_spacing, mark_from_rpr_child};
use crate::generic_font;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Block attr holding the unmodeled `w:pPr` children of an imported paragraph, verbatim.
pub(super) const ATTR_PPR: &str = "docx_ppr";

/// Mark holding the unmodeled `w:rPr` children of the runs it covers, verbatim.
pub(crate) const MARK_RPR: &str = "docx_rpr";

/// `CT_RPr`'s child order.
const RPR_ORDER: &[&[u8]] = &[
    b"w:rStyle",
    b"w:rFonts",
    b"w:b",
    b"w:bCs",
    b"w:i",
    b"w:iCs",
    b"w:caps",
    b"w:smallCaps",
    b"w:strike",
    b"w:dstrike",
    b"w:outline",
    b"w:shadow",
    b"w:emboss",
    b"w:imprint",
    b"w:noProof",
    b"w:snapToGrid",
    b"w:vanish",
    b"w:webHidden",
    b"w:color",
    b"w:spacing",
    b"w:w",
    b"w:kern",
    b"w:position",
    b"w:sz",
    b"w:szCs",
    b"w:highlight",
    b"w:u",
    b"w:effect",
    b"w:bdr",
    b"w:shd",
    b"w:fitText",
    b"w:vertAlign",
    b"w:rtl",
    b"w:cs",
    b"w:em",
    b"w:lang",
    b"w:eastAsianLayout",
    b"w:specVanish",
    b"w:oMath",
    b"w:rPrChange",
];

/// The unmodeled children of a run's `w:rPr` (its verbatim XML), concatenated; empty when
/// the model represents all of them. `w:rFonts` is always kept: the model only knows the
/// font's generic family, so export writes the original names back while that still agrees.
pub(super) fn kept_run_props(rpr: &str) -> String {
    let inner = rpr
        .find('>')
        .filter(|at| !rpr.get(..*at).unwrap_or_default().ends_with('/'))
        .and_then(|at| rpr.get(at.saturating_add(1)..rpr.len().saturating_sub("</w:rPr>".len())))
        .unwrap_or_default();
    let mut kept = String::new();
    for (name, raw) in children(inner) {
        let modeled = match name.as_slice() {
            b"w:rFonts" => false,
            // Written together with `w:sz`.
            b"w:szCs" => true,
            _ => first_start(&raw).is_some_and(|e| mark_from_rpr_child(&e).is_some()),
        };
        if !modeled {
            kept.push_str(&raw);
        }
    }
    kept
}

/// The start tag of an element's XML, parsed.
fn first_start(raw: &str) -> Option<BytesStart<'static>> {
    let mut reader = Reader::from_str(raw);
    match reader.read_event().ok()? {
        Event::Start(e) | Event::Empty(e) => Some(e.into_owned()),
        _ => None,
    }
}

/// A run's `w:rPr` inner XML from the `modeled` properties the exporter writes and the ones
/// kept from import, in `CT_RPr` order. A modeled child replaces a kept one of the same name,
/// except the kept `w:rFonts` while its family is still the modeled one.
pub(super) fn run_props_xml(modeled: &str, kept: Option<&str>) -> String {
    let Some(kept) = kept.filter(|k| !k.is_empty()) else {
        return modeled.to_string();
    };
    let family = |raw: &str| {
        first_start(raw)
            .and_then(|e| attr_val(&e, b"w:ascii").or_else(|| attr_val(&e, b"w:hAnsi")))
            .map(|f| generic_font(&f))
    };
    let mut all = children(modeled);
    for (name, raw) in children(kept) {
        if let Some(slot) = all.iter_mut().find(|(n, _)| *n == name) {
            if name == b"w:rFonts" && family(&raw) == family(&slot.1) {
                slot.1 = raw;
            }
            continue;
        }
        if name == b"w:rFonts" {
            // The font family was removed in the model.
            continue;
        }
        all.push((name, raw));
    }
    let order = |name: &[u8]| {
        RPR_ORDER
            .iter()
            .position(|n| *n == name)
            .unwrap_or(RPR_ORDER.len())
    };
    all.sort_by_key(|(name, _)| order(name));
    all.into_iter().map(|(_, xml)| xml).collect()
}

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

/// Block attrs keeping a table's properties verbatim: the table's `w:tblPr` and `w:tblGrid` on
/// its first cell, a row's `w:trPr` on the row's first cell, and a cell's `w:tcPr` children
/// (less the spans the model holds) on the cell.
pub(super) const ATTR_TBLPR: &str = "docx_tblpr";
pub(super) const ATTR_TBLGRID: &str = "docx_tblgrid";
pub(super) const ATTR_TRPR: &str = "docx_trpr";
pub(super) const ATTR_TCPR: &str = "docx_tcpr";

/// The table properties and cell width the exporter writes when it has none kept; imported as
/// such they carry nothing to keep.
pub(super) const DEFAULT_TBLPR: &str = r#"<w:tblPr><w:tblW w:w="0" w:type="auto"/></w:tblPr>"#;
pub(super) const DEFAULT_TCW: &str = r#"<w:tcW w:w="0" w:type="auto"/>"#;

/// Whether a table's properties or grid, as imported, say more than the exporter's defaults
/// (automatic width, columns without widths).
pub(super) fn is_informative_table_part(raw: &str) -> bool {
    raw != DEFAULT_TBLPR
        && !(raw.starts_with("<w:tblGrid")
            && element_children(raw)
                .iter()
                .all(|(_, col)| col == "<w:gridCol/>"))
}

/// `CT_TcPr`'s child order.
const TCPR_ORDER: &[&[u8]] = &[
    b"w:cnfStyle",
    b"w:tcW",
    b"w:gridSpan",
    b"w:hMerge",
    b"w:vMerge",
    b"w:tcBorders",
    b"w:shd",
    b"w:noWrap",
    b"w:tcMar",
    b"w:textDirection",
    b"w:tcFitText",
    b"w:vAlign",
    b"w:hideMark",
    b"w:headers",
    b"w:cellIns",
    b"w:cellDel",
    b"w:cellMerge",
    b"w:tcPrChange",
];

/// The children of a cell's `w:tcPr` (its verbatim XML) the model does not hold: all but the
/// spans (`w:gridSpan`, `w:vMerge`).
pub(super) fn kept_cell_props(tcpr: &str) -> String {
    element_children(tcpr)
        .into_iter()
        .filter(|(name, raw)| {
            !matches!(name.as_slice(), b"w:gridSpan" | b"w:vMerge") && raw != DEFAULT_TCW
        })
        .map(|(_, raw)| raw)
        .collect()
}

/// A cell's `w:tcPr` from the `modeled` spans and the kept properties; a kept `w:tcW` (the
/// cell's width) replaces the exporter's automatic one.
pub(super) fn cell_props_xml(modeled: &[(&[u8], String)], kept: Option<&str>) -> String {
    let mut all: Vec<(Vec<u8>, String)> = modeled
        .iter()
        .filter(|(_, xml)| !xml.is_empty())
        .map(|(name, xml)| (name.to_vec(), xml.clone()))
        .collect();
    for (name, raw) in kept.map(children).unwrap_or_default() {
        match all.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) if name == b"w:tcW" => slot.1 = raw,
            Some(_) => {}
            None => all.push((name, raw)),
        }
    }
    let order = |name: &[u8]| {
        TCPR_ORDER
            .iter()
            .position(|n| *n == name)
            .unwrap_or(TCPR_ORDER.len())
    };
    all.sort_by_key(|(name, _)| order(name));
    let inner: String = all.into_iter().map(|(_, xml)| xml).collect();
    format!("<w:tcPr>{inner}</w:tcPr>")
}

/// How many `w:gridCol`s a kept `w:tblGrid` has.
pub(super) fn grid_columns(tblgrid: &str) -> usize {
    element_children(tblgrid)
        .iter()
        .filter(|(name, _)| name == b"w:gridCol")
        .count()
}

/// The children of one element's verbatim XML.
fn element_children(xml: &str) -> Vec<(Vec<u8>, String)> {
    let Some(open_end) = xml.find('>') else {
        return Vec::new();
    };
    if xml.get(..open_end).unwrap_or_default().ends_with('/') {
        return Vec::new();
    }
    let close = xml.rfind("</").unwrap_or(xml.len());
    children(
        xml.get(open_end.saturating_add(1)..close)
            .unwrap_or_default(),
    )
}

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
/// (numbering, alignment unless `keep_jc`); the `w:spacing` without the line spacing the
/// model holds.
pub(super) fn kept_child(e: &BytesStart, raw: &str, keep_jc: bool) -> Option<String> {
    match e.name().as_ref() {
        b"w:numPr" => None,
        b"w:jc" if !keep_jc => None,
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
    fn keeps_unmodeled_run_properties_and_original_font_names() {
        let rpr = r#"<w:rPr><w:rStyle w:val="Keyword"/><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif"/><w:b/><w:caps/><w:sz w:val="24"/><w:szCs w:val="24"/></w:rPr>"#;
        let kept = kept_run_props(rpr);
        assert_eq!(
            kept,
            r#"<w:rStyle w:val="Keyword"/><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif"/><w:caps/>"#
        );
        let modeled = r#"<w:rFonts w:ascii="Times New Roman" w:hAnsi="Times New Roman"/><w:b/><w:sz w:val="24"/><w:szCs w:val="24"/>"#;
        assert_eq!(
            run_props_xml(modeled, Some(&kept)),
            r#"<w:rStyle w:val="Keyword"/><w:rFonts w:ascii="Liberation Serif" w:hAnsi="Liberation Serif"/><w:b/><w:caps/><w:sz w:val="24"/><w:szCs w:val="24"/>"#
        );
        // A changed family replaces the kept font; a removed one drops it.
        let mono = r#"<w:rFonts w:ascii="Courier New" w:hAnsi="Courier New"/>"#;
        assert!(run_props_xml(mono, Some(&kept)).contains("Courier New"));
        assert!(!run_props_xml("", Some(&kept)).contains("rFonts"));
        assert_eq!(kept_run_props("<w:rPr/>"), "");
    }

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
