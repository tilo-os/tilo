//! Parse ODF styles — named styles from `styles.xml` and automatic styles from
//! `content.xml` — into a registry the body parser resolves against. Real `.odt`
//! files keep most formatting (headings, bold, fonts, list numbering) in named
//! styles, so resolving them is what makes real documents import faithfully.

use crate::document::MarkValue;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::HashMap;

pub(super) fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

pub(super) fn parse_pt(v: &str) -> Option<i64> {
    v.trim_end_matches("pt").trim().parse::<f64>().ok().map(|f| f.round() as i64)
}

pub(super) fn generic_font(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    if n.contains("mono") || n.contains("courier") || n.contains("consol") {
        "mono".into()
    } else if n.contains("serif") && !n.contains("sans") {
        "serif".into()
    } else if n.contains("times") || n.contains("georgia") || n.contains("garamond") || n.contains("roman") {
        "serif".into()
    } else {
        "sans".into()
    }
}

/// Canonicalise an ODF `fo:color` (`#RRGGBB`) to lowercase so that the round-trip
/// is stable and two replicas that imported the same colour converge.
pub(super) fn normalize_color(v: &str) -> String {
    let t = v.trim();
    if let Some(hex) = t.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return format!("#{}", hex.to_ascii_lowercase());
        }
    }
    t.to_string()
}

pub(super) fn map_align(v: &str) -> String {
    match v {
        "center" => "center".into(),
        "end" | "right" => "right".into(),
        _ => "left".into(),
    }
}

/// Inline marks (with values) implied by a `style:text-properties` element.
pub(super) fn marks_from_text_props(e: &BytesStart) -> Vec<(String, MarkValue)> {
    let mut m = Vec::new();
    for a in e.attributes().flatten() {
        let val = a.unescape_value().unwrap_or_default();
        match a.key.as_ref() {
            b"fo:font-weight" if val == "bold" => m.push(("strong".into(), MarkValue::Bool(true))),
            b"fo:font-style" if val == "italic" => m.push(("em".into(), MarkValue::Bool(true))),
            b"style:text-underline-style" if val != "none" => m.push(("underline".into(), MarkValue::Bool(true))),
            b"style:text-line-through-style" if val != "none" => m.push(("strike".into(), MarkValue::Bool(true))),
            b"fo:font-size" => {
                if let Some(n) = parse_pt(&val) {
                    m.push(("fontSize".into(), MarkValue::Int(n)));
                }
            }
            b"style:font-name" | b"fo:font-family" => m.push(("fontFamily".into(), MarkValue::Str(generic_font(&val)))),
            b"fo:color" => m.push(("color".into(), MarkValue::Str(normalize_color(&val)))),
            _ => {}
        }
    }
    m
}

#[derive(Default, Clone)]
pub(super) struct ParaStyle {
    pub parent: Option<String>,
    pub align: Option<String>,
    pub outline_level: Option<i64>,
    pub heading_name: bool,
    pub name_level: Option<i64>,
    pub text: Vec<(String, MarkValue)>,
}

#[derive(Default)]
pub(super) struct StyleRegistry {
    /// `text`-family style name → inline marks.
    pub text: HashMap<String, Vec<(String, MarkValue)>>,
    /// `paragraph`-family style name → paragraph style.
    pub para: HashMap<String, ParaStyle>,
    /// list-style name → is it numbered (ordered)?
    pub list_ordered: HashMap<String, bool>,
}

impl StyleRegistry {
    /// Heading level for a paragraph style, following parent chains (LibreOffice
    /// derives custom styles from "Heading N").
    pub(super) fn heading_level(&self, name: &str) -> Option<i64> {
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let n = cur?;
            let p = self.para.get(&n)?;
            if let Some(l) = p.outline_level {
                return Some(l.clamp(1, 6));
            }
            if p.heading_name {
                return Some(p.name_level.unwrap_or(1).clamp(1, 6));
            }
            cur = p.parent.clone();
        }
        None
    }

    pub(super) fn para_align(&self, name: &str) -> Option<String> {
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let n = cur?;
            let p = self.para.get(&n)?;
            if let Some(a) = &p.align {
                return Some(a.clone());
            }
            cur = p.parent.clone();
        }
        None
    }

    /// Inline marks a paragraph style applies to its whole text (parent-first so
    /// the most-derived style wins).
    pub(super) fn para_text_marks(&self, name: &str) -> Vec<(String, MarkValue)> {
        let mut chain: Vec<&ParaStyle> = Vec::new();
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let Some(n) = cur else { break };
            let Some(p) = self.para.get(&n) else { break };
            chain.push(p);
            cur = p.parent.clone();
        }
        let mut out: Vec<(String, MarkValue)> = Vec::new();
        for p in chain.iter().rev() {
            for (k, v) in &p.text {
                out.retain(|(k2, _)| k2 != k);
                out.push((k.clone(), v.clone()));
            }
        }
        out
    }

    pub(super) fn text_marks(&self, name: &str) -> Vec<(String, MarkValue)> {
        self.text.get(name).cloned().unwrap_or_default()
    }
}

/// `(looks like a heading, level from the name)` for a style name/display name.
fn heading_from_name(name: &str, display: Option<&str>) -> (bool, Option<i64>) {
    for c in [name, display.unwrap_or("")] {
        let c = c.replace("_20_", " ");
        if c.to_ascii_lowercase().trim_start().starts_with("heading") {
            let lvl = c.split(|ch: char| !ch.is_ascii_digit()).find_map(|t| t.parse::<i64>().ok());
            return (true, lvl);
        }
    }
    (false, None)
}

/// Parse all `style:style` + `text:list-style` from an ODF xml stream into `reg`.
/// Safe to call repeatedly (styles.xml then content.xml) — later styles win.
pub(super) fn parse_styles(xml: &[u8], reg: &mut StyleRegistry) {
    let Ok(s) = std::str::from_utf8(xml) else { return };
    let mut reader = Reader::from_str(s);

    let mut name: Option<String> = None;
    let mut family: Option<String> = None;
    let mut parent: Option<String> = None;
    let mut outline: Option<i64> = None;
    let mut heading_name = false;
    let mut name_level: Option<i64> = None;
    let mut align: Option<String> = None;
    let mut text: Vec<(String, MarkValue)> = Vec::new();

    let mut list_name: Option<String> = None;
    let mut list_numbered = false;

    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            // A self-closing <style:style/> (no properties) has no End event, so
            // register it straight from its attributes (parent/outline/heading).
            Event::Empty(e) if e.name().as_ref() == b"style:style" => {
                if let (Some(nm), Some(fam)) = (attr_val(&e, b"style:name"), attr_val(&e, b"style:family")) {
                    if fam == "paragraph" {
                        let display = attr_val(&e, b"style:display-name");
                        let (h, l) = heading_from_name(&nm, display.as_deref());
                        reg.para.insert(
                            nm,
                            ParaStyle {
                                parent: attr_val(&e, b"style:parent-style-name"),
                                align: None,
                                outline_level: attr_val(&e, b"style:default-outline-level").and_then(|v| v.parse().ok()),
                                heading_name: h,
                                name_level: l,
                                text: Vec::new(),
                            },
                        );
                    }
                }
            }
            Event::Start(e) | Event::Empty(e) => match e.name().as_ref() {
                b"style:style" => {
                    name = attr_val(&e, b"style:name");
                    family = attr_val(&e, b"style:family");
                    parent = attr_val(&e, b"style:parent-style-name");
                    outline = attr_val(&e, b"style:default-outline-level").and_then(|v| v.parse().ok());
                    let display = attr_val(&e, b"style:display-name");
                    let (h, l) = heading_from_name(name.as_deref().unwrap_or(""), display.as_deref());
                    heading_name = h;
                    name_level = l;
                    align = None;
                    text = Vec::new();
                }
                b"style:text-properties" => text.extend(marks_from_text_props(&e)),
                b"style:paragraph-properties" => {
                    if let Some(a) = attr_val(&e, b"fo:text-align") {
                        align = Some(map_align(&a));
                    }
                }
                b"text:list-style" => {
                    list_name = attr_val(&e, b"style:name");
                    list_numbered = false;
                }
                b"text:list-level-style-number" => list_numbered = true,
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"style:style" => {
                    if let (Some(n), Some(fam)) = (name.take(), family.take()) {
                        match fam.as_str() {
                            "text" => {
                                if !text.is_empty() {
                                    reg.text.insert(n, std::mem::take(&mut text));
                                }
                            }
                            "paragraph" => {
                                reg.para.insert(
                                    n,
                                    ParaStyle {
                                        parent: parent.take(),
                                        align: align.take(),
                                        outline_level: outline.take(),
                                        heading_name,
                                        name_level: name_level.take(),
                                        text: std::mem::take(&mut text),
                                    },
                                );
                            }
                            _ => {}
                        }
                    }
                    parent = None;
                    outline = None;
                    heading_name = false;
                    text.clear();
                }
                b"text:list-style" => {
                    if let Some(n) = list_name.take() {
                        reg.list_ordered.insert(n, list_numbered);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_named_heading_and_text_styles() {
        let xml = br#"<office:document-styles>
        <office:styles>
        <style:style style:name="Heading_20_1" style:display-name="Heading 1" style:family="paragraph" style:default-outline-level="1"><style:paragraph-properties fo:text-align="center"/><style:text-properties fo:font-weight="bold"/></style:style>
        <style:style style:name="MySub" style:family="paragraph" style:parent-style-name="Heading_20_1"/>
        <style:style style:name="Strong" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
        </office:styles>
        </office:document-styles>"#;
        let mut reg = StyleRegistry::default();
        parse_styles(xml, &mut reg);
        assert_eq!(reg.heading_level("Heading_20_1"), Some(1));
        assert_eq!(reg.heading_level("MySub"), Some(1), "inherits heading via parent");
        assert_eq!(reg.para_align("Heading_20_1").as_deref(), Some("center"));
        assert!(reg.text_marks("Strong").iter().any(|(n, _)| n == "strong"));
        assert!(reg.para_text_marks("Heading_20_1").iter().any(|(n, _)| n == "strong"));
    }

    #[test]
    fn detects_ordered_list_style() {
        let xml = br#"<office:automatic-styles>
        <text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style>
        <text:list-style style:name="L2"><text:list-level-style-bullet text:level="1"/></text:list-style>
        </office:automatic-styles>"#;
        let mut reg = StyleRegistry::default();
        parse_styles(xml, &mut reg);
        assert_eq!(reg.list_ordered.get("L1"), Some(&true));
        assert_eq!(reg.list_ordered.get("L2"), Some(&false));
    }
}
