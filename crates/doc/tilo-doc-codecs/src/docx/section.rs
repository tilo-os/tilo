//! The body's final `w:sectPr` and the header/footer parts it references, kept consistent
//! across an export.
//!
//! The canonical model holds one page header and one page footer; they are exported as the
//! section's `default` header/footer, under the fixed relationship ids
//! [`HEADER_REL_ID`]/[`FOOTER_REL_ID`]. An imported `.docx` also carries, verbatim:
//!
//! - its final `w:sectPr` (captured by the body parser as a fidelity block), whose page setup
//!   (`w:pgSz`, `w:pgMar`, `w:titlePg`, …) is kept, and whose `first`/`even` header and footer
//!   references are kept pointing at their original parts — qdoc does not model those
//!   variants, so they pass through unedited;
//! - the package's relationships, parts and content types.
//!
//! On export the section's `default` references are replaced by qdoc's, `w:cols` by the
//! model's column setting, and every header/footer relationship no exported `w:sectPr`
//! references any more is removed together with its part, that part's relationships and its
//! content-type override.
//!
//! Section breaks inside the body (a `w:sectPr` in a paragraph's `w:pPr`) are kept verbatim
//! with their paragraph (see `paragraph`), including their own page setup and columns. Their
//! header/footer references get the same treatment as the final section's — `default` points
//! at qdoc's header/footer (the model has one for the whole document), `first`/`even` stay
//! while their part does — so every section shows the document's header and footer.

use super::headers::{ExportedParts, FOOTER_REL_ID, HEADER_REL_ID, KeptRegions, normalize_target};
use super::styles::attr_val;
use crate::package::ZipPackage;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeSet;
use std::fmt::Write as _;

pub(super) const DOCUMENT_RELS: &str = "word/_rels/document.xml.rels";
pub(super) const CONTENT_TYPES: &str = "[Content_Types].xml";
pub(super) const HEADER_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml";
pub(super) const FOOTER_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml";
pub(super) const HEADER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
pub(super) const FOOTER_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

/// `CT_SectPr`'s child order after the header/footer references; `w:cols` goes between
/// `w:lnNumType`/`w:pgNumType` and `w:formProt`.
const BEFORE_COLS: &[&[u8]] = &[
    b"w:footnotePr",
    b"w:endnotePr",
    b"w:type",
    b"w:pgSz",
    b"w:pgMar",
    b"w:paperSrc",
    b"w:pgBorders",
    b"w:lnNumType",
    b"w:pgNumType",
];

/// Whether a fidelity block's XML is a `w:sectPr` — the section properties, which export
/// rebuilds rather than copying into the body.
pub(super) fn is_section_properties(xml: &str) -> bool {
    xml.trim_start()
        .strip_prefix("<w:sectPr")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c == '>' || c == '/' || c.is_whitespace())
}

/// Which region a reference element is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Header,
    Footer,
}

impl Kind {
    fn of_rel_type(ty: &str) -> Option<Self> {
        // Transitional and Strict OOXML share the `…/relationships/header` suffix.
        if ty.ends_with("/relationships/header") {
            Some(Self::Header)
        } else if ty.ends_with("/relationships/footer") {
            Some(Self::Footer)
        } else {
            None
        }
    }

    fn of_element(name: &[u8]) -> Option<Self> {
        match name {
            b"w:headerReference" => Some(Self::Header),
            b"w:footerReference" => Some(Self::Footer),
            _ => None,
        }
    }
}

/// One `Relationship` of `word/_rels/document.xml.rels`.
struct Rel {
    id: String,
    kind: Option<Kind>,
    part: String,
    external: bool,
}

fn document_rels(c: &ZipPackage) -> Vec<Rel> {
    let Some(xml) = c
        .get(DOCUMENT_RELS)
        .and_then(|b| std::str::from_utf8(b).ok())
    else {
        return Vec::new();
    };
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Relationship" => {
                if let (Some(id), Some(ty), Some(target)) = (
                    attr_val(&e, b"Id"),
                    attr_val(&e, b"Type"),
                    attr_val(&e, b"Target"),
                ) {
                    out.push(Rel {
                        id,
                        kind: Kind::of_rel_type(&ty),
                        part: normalize_target(&target),
                        external: attr_val(&e, b"TargetMode").as_deref() == Some("External"),
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// A `w:headerReference`/`w:footerReference`.
struct Reference {
    kind: Kind,
    /// `w:type="default"`, or no `w:type` at all.
    is_default: bool,
    id: String,
}

/// A top-level child of the original `w:sectPr`: its element name and verbatim XML.
struct Child {
    name: Vec<u8>,
    raw: String,
    reference: Option<Reference>,
}

/// The original `w:sectPr`'s start tag (`None` when self-closing) and its top-level children.
fn split_sectpr(xml: &str) -> (Option<String>, Vec<Child>) {
    let mut reader = Reader::from_str(xml);
    let mut open: Option<String> = None;
    let mut children = Vec::new();
    let mut depth = 0usize;
    // Start offset of the non-empty child being read, and that child (its `raw` filled at its end).
    let mut current: Option<(usize, Child)> = None;
    loop {
        let before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else { break };
        let after = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let raw = || xml.get(before..after).unwrap_or_default().to_string();
        let reference = |e: &BytesStart| {
            Kind::of_element(e.name().as_ref()).map(|kind| Reference {
                kind,
                is_default: attr_val(e, b"w:type").is_none_or(|t| t == "default"),
                id: attr_val(e, b"r:id").unwrap_or_default(),
            })
        };
        match ev {
            Event::Start(e) => {
                if depth == 0 {
                    open = Some(raw());
                } else if depth == 1 {
                    current = Some((
                        before,
                        Child {
                            name: e.name().as_ref().to_vec(),
                            raw: String::new(),
                            reference: reference(&e),
                        },
                    ));
                }
                depth = depth.saturating_add(1);
            }
            Event::Empty(e) => {
                if depth == 1 {
                    children.push(Child {
                        name: e.name().as_ref().to_vec(),
                        raw: raw(),
                        reference: reference(&e),
                    });
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 1
                    && let Some((start, mut child)) = current.take()
                {
                    child.raw = xml.get(start..after).unwrap_or_default().to_string();
                    children.push(child);
                }
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    (open, children)
}

/// Remove every top-level `tag` element of `xml` for which `drop` holds.
pub(super) fn remove_elements(
    xml: &str,
    tag: &[u8],
    mut drop: impl FnMut(&BytesStart) -> bool,
) -> String {
    let mut reader = Reader::from_str(xml);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<usize> = None;
    loop {
        let before = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        let Ok(ev) = reader.read_event() else { break };
        let after = usize::try_from(reader.buffer_position()).unwrap_or(usize::MAX);
        match ev {
            Event::Empty(e) if e.name().as_ref() == tag && drop(&e) => spans.push((before, after)),
            Event::Start(e) if e.name().as_ref() == tag && drop(&e) => open = Some(before),
            Event::End(e) if e.name().as_ref() == tag => {
                if let Some(start) = open.take() {
                    spans.push((start, after));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut out = String::with_capacity(xml.len());
    let mut at = 0;
    for (start, end) in spans {
        out.push_str(xml.get(at..start).unwrap_or_default());
        at = end;
    }
    out.push_str(xml.get(at..).unwrap_or_default());
    out
}

/// `word/header1.xml` → `word/_rels/header1.xml.rels`.
fn part_rels_name(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// Drop every header/footer relationship whose id is not in `keep`, then every part only
/// those relationships reached, with its relationships part and content-type override.
fn prune_header_footer_rels(c: &mut ZipPackage, rels: &[Rel], keep: &BTreeSet<String>) {
    let dropped: BTreeSet<&str> = rels
        .iter()
        .filter(|r| r.kind.is_some() && !keep.contains(&r.id))
        .map(|r| r.id.as_str())
        .collect();
    if !dropped.is_empty()
        && let Some(xml) = c
            .get(DOCUMENT_RELS)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    {
        let pruned = remove_elements(&xml, b"Relationship", |e| {
            attr_val(e, b"Id").is_some_and(|id| dropped.contains(id.as_str()))
        });
        c.set(DOCUMENT_RELS, pruned.into_bytes());
    }
    let still_reached: BTreeSet<&str> = rels
        .iter()
        .filter(|r| !r.external && !dropped.contains(r.id.as_str()))
        .map(|r| r.part.as_str())
        .collect();
    // Parts the dropped relationships reached, plus any `word/` header/footer part the
    // content types still declare that no relationship reaches (left behind by an earlier
    // export). Header/footer parts elsewhere (e.g. `word/glossary/`) belong to other rels.
    let content_types = c
        .get(CONTENT_TYPES)
        .map(|b| String::from_utf8_lossy(b).into_owned());
    let declared = content_types
        .as_deref()
        .map(header_footer_overrides)
        .unwrap_or_default();
    let orphans: BTreeSet<String> = rels
        .iter()
        .filter(|r| !r.external && dropped.contains(r.id.as_str()))
        .map(|r| r.part.clone())
        .chain(
            declared
                .into_iter()
                .filter(|p| p.rsplit_once('/').is_some_and(|(dir, _)| dir == "word")),
        )
        .filter(|p| !still_reached.contains(p.as_str()))
        .collect();
    for part in &orphans {
        c.remove(part);
        c.remove(&part_rels_name(part));
    }
    if !orphans.is_empty()
        && let Some(xml) = content_types
    {
        let pruned = remove_elements(&xml, b"Override", |e| {
            attr_val(e, b"PartName").is_some_and(|p| orphans.contains(p.trim_start_matches('/')))
        });
        c.set(CONTENT_TYPES, pruned.into_bytes());
    }
}

/// Part names (without the leading `/`) the content types declare as headers or footers.
fn header_footer_overrides(content_types: &str) -> Vec<String> {
    let mut reader = Reader::from_str(content_types);
    let mut out = Vec::new();
    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"Override" => {
                let ct = attr_val(&e, b"ContentType");
                if matches!(
                    ct.as_deref(),
                    Some(HEADER_CONTENT_TYPE | FOOTER_CONTENT_TYPE)
                ) && let Some(part) = attr_val(&e, b"PartName")
                {
                    out.push(part.trim_start_matches('/').to_string());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

/// The first `word/{stem}N.xml` not already in the package. One more candidate than there
/// are parts, so one is always free.
fn free_part_name(c: &ZipPackage, stem: &str) -> String {
    (1..=c.parts.len().saturating_add(1))
        .map(|n| format!("word/{stem}{n}.xml"))
        .find(|name| c.get(name).is_none())
        .unwrap_or_else(|| format!("word/{stem}.xml"))
}

/// Where this export writes its header/footer, and the `w:sectPr` that references them.
pub(super) struct SectionPlan {
    /// The complete final `w:sectPr`, or empty when the document needs none.
    pub sectpr: String,
    /// The part qdoc's header is written to, when there is a header.
    pub header_part: Option<String>,
    /// The part qdoc's footer is written to, when there is a footer.
    pub footer_part: Option<String>,
    /// The `first`/`even` header/footer relationships kept (they still resolve).
    kept_refs: BTreeSet<String>,
}

impl SectionPlan {
    /// The header/footer references a rebuilt `w:sectPr` starts with: qdoc's `default` pair,
    /// then the kept ones of `children` (`first`/`even`, and the `default` ones of a region
    /// whose original part is kept).
    fn references(&self, children: &[Child]) -> String {
        let mut inner = String::new();
        if self.header_part.is_some() {
            let _ = write!(
                inner,
                r#"<w:headerReference w:type="default" r:id="{HEADER_REL_ID}"/>"#
            );
        }
        if self.footer_part.is_some() {
            let _ = write!(
                inner,
                r#"<w:footerReference w:type="default" r:id="{FOOTER_REL_ID}"/>"#
            );
        }
        for ch in children {
            if let Some(r) = &ch.reference
                && self.kept_refs.contains(&r.id)
            {
                inner.push_str(&ch.raw);
            }
        }
        inner
    }

    /// A section break's `w:sectPr` inside the body, for this export: its header/footer
    /// references rebuilt like the final section's, everything else (page setup, columns) kept.
    pub fn section_break(&self, xml: &str) -> String {
        let (open, children) = split_sectpr(xml);
        let mut inner = self.references(&children);
        for ch in children.iter().filter(|ch| ch.reference.is_none()) {
            inner.push_str(&ch.raw);
        }
        let open = open.unwrap_or_else(|| "<w:sectPr>".to_string());
        format!("{open}{inner}</w:sectPr>")
    }
}

/// Rebuild the section properties around qdoc's header/footer and columns, and prune `c`
/// (holding the carried-through parts) of header/footer relationships and parts the new
/// `w:sectPr` does not reference. `original` is the imported `w:sectPr`, if any.
pub(super) fn plan(
    c: &mut ZipPackage,
    original: Option<&str>,
    section_breaks: &[&str],
    parts: &ExportedParts,
    keep: KeptRegions,
    cols: &str,
) -> SectionPlan {
    let rels = document_rels(c);
    let (open, children) = original.map(split_sectpr).unwrap_or_default();

    // A `first`/`even` reference, of the final section or a section break, stays only while
    // its relationship and part are still there.
    let resolves = |kind: Kind, id: &str| {
        rels.iter()
            .any(|r| r.id == id && r.kind == Some(kind) && !r.external && c.get(&r.part).is_some())
    };
    let break_children: Vec<Child> = section_breaks
        .iter()
        .flat_map(|xml| split_sectpr(xml).1)
        .collect();
    let kept_default = |kind: Kind| match kind {
        Kind::Header => keep.header,
        Kind::Footer => keep.footer,
    };
    let kept_refs: BTreeSet<String> = children
        .iter()
        .chain(&break_children)
        .filter_map(|ch| ch.reference.as_ref())
        .filter(|r| (!r.is_default || kept_default(r.kind)) && resolves(r.kind, &r.id))
        .map(|r| r.id.clone())
        .collect();
    prune_header_footer_rels(c, &rels, &kept_refs);

    let mut plan = SectionPlan {
        sectpr: String::new(),
        header_part: parts.header.as_ref().map(|_| free_part_name(c, "header")),
        footer_part: parts.footer.as_ref().map(|_| free_part_name(c, "footer")),
        kept_refs,
    };
    let mut inner = plan.references(&children);
    let mut cols_written = cols.is_empty();
    for ch in children
        .iter()
        .filter(|ch| ch.reference.is_none() && ch.name != b"w:cols")
    {
        if !cols_written && !BEFORE_COLS.contains(&ch.name.as_slice()) {
            inner.push_str(cols);
            cols_written = true;
        }
        inner.push_str(&ch.raw);
    }
    if !cols_written {
        inner.push_str(cols);
    }

    plan.sectpr = match (open, inner.is_empty()) {
        (Some(open), _) => format!("{open}{inner}</w:sectPr>"),
        (None, true) if original.is_none() => String::new(),
        (None, _) => format!("<w:sectPr>{inner}</w:sectPr>"),
    };
    plan
}

/// The `Relationship` element wiring `part` (a `word/…` name) in the document rels.
pub(super) fn relationship_xml(id: &str, rel_type: &str, part: &str) -> String {
    format!(
        r#"<Relationship Id="{id}" Type="{rel_type}" Target="{}"/>"#,
        crate::xml_escape::escape_attr(part.strip_prefix("word/").unwrap_or(part))
    )
}

/// The content-type `Override` element for `part`.
pub(super) fn override_xml(part: &str, content_type: &str) -> String {
    format!(
        r#"<Override PartName="/{}" ContentType="{content_type}"/>"#,
        crate::xml_escape::escape_attr(part)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_properties_are_recognised_rebuilt_and_removed() {
        // Phase `recognises_section_properties`.
        {
            assert!(is_section_properties("<w:sectPr>"));
            assert!(is_section_properties(
                r#"<w:sectPr w:rsidR="00A1"><w:pgSz/></w:sectPr>"#
            ));
            assert!(is_section_properties("<w:sectPr/>"));
            assert!(!is_section_properties("<w:sectPrChange/>"));
            assert!(!is_section_properties("<w:sdt></w:sdt>"));
        }
        // Phase `rebuilt_sectpr_keeps_page_setup_and_orders_cols`.
        {
            let original = concat!(
                r#"<w:sectPr w:rsidR="00A1">"#,
                r#"<w:headerReference w:type="default" r:id="rId8"/>"#,
                r#"<w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440"/>"#,
                r#"<w:cols w:space="708"/><w:titlePg/><w:docGrid w:linePitch="360"/>"#,
                "</w:sectPr>",
            );
            let mut c = ZipPackage { parts: Vec::new() };
            let parts = ExportedParts {
                header: Some(Vec::new()),
                footer: None,
            };
            let plan = plan(
                &mut c,
                Some(original),
                &[],
                &parts,
                KeptRegions::default(),
                r#"<w:cols w:num="2"/>"#,
            );
            assert_eq!(
                plan.sectpr,
                concat!(
                    r#"<w:sectPr w:rsidR="00A1">"#,
                    r#"<w:headerReference w:type="default" r:id="rIdQdocHdr"/>"#,
                    r#"<w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440"/>"#,
                    r#"<w:cols w:num="2"/><w:titlePg/><w:docGrid w:linePitch="360"/>"#,
                    "</w:sectPr>",
                )
            );
            assert_eq!(plan.header_part.as_deref(), Some("word/header1.xml"));
            assert_eq!(plan.footer_part, None);
        }
        // Phase `removes_only_matching_elements`.
        {
            let xml = r#"<R><X Id="a"/><X Id="b"></X><Y Id="a"/></R>"#;
            assert_eq!(
                remove_elements(xml, b"X", |e| attr_val(e, b"Id").is_some()),
                r#"<R><Y Id="a"/></R>"#
            );
        }
    }
}
