//! Parse OOXML styles (`word/styles.xml`) and list numbering
//! (`word/numbering.xml`) into a registry the body parser resolves against.
//! Real `.docx` files keep most formatting (headings, bold, fonts) in named
//! styles and list ordering in numbering definitions, so resolving them is what
//! makes real documents import faithfully.

use crate::document::MarkValue;
use crate::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME, STYLE_SIZE, STYLE_SPACE_AFTER,
    STYLE_SPACE_BEFORE,
};
use crate::snapshot::AttrVal;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::{BTreeMap, HashMap};

pub(super) fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

/// OOXML toggle property (`<w:b/>`, `<w:i/>`, …): present means *on* unless its
/// `w:val` explicitly turns it off (used to override an inherited style).
pub(super) fn toggle_on(e: &BytesStart) -> bool {
    match attr_val(e, b"w:val") {
        None => true,
        Some(v) => !matches!(v.as_str(), "false" | "0" | "off" | "none"),
    }
}

/// `<w:u>` is on for any underline style other than `none`.
pub(super) fn underline_on(e: &BytesStart) -> bool {
    match attr_val(e, b"w:val") {
        Some(v) => v != "none" && v != "0" && v != "false",
        None => true,
    }
}

/// `<w:sz w:val="N"/>` is in half-points; the model stores whole points.
pub(super) fn parse_half_point(e: &BytesStart) -> Option<i64> {
    attr_val(e, b"w:val")
        .and_then(|v| v.parse::<f64>().ok())
        .map(|hp| (hp / 2.0).round() as i64)
}

/// Collapse a concrete font name onto one of the model's generic families.
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

/// `<w:jc w:val>` → the model's `align` value.
pub(super) fn map_jc(v: &str) -> String {
    match v {
        "center" => "center".into(),
        "end" | "right" => "right".into(),
        _ => "left".into(),
    }
}

/// Normalise an OOXML `w:color w:val` (`RRGGBB`, optionally `#`-prefixed) to the
/// model's canonical `#rrggbb`. `auto`/non-hex values yield `None` so the style's
/// colour stays at its neutral default rather than importing garbage.
pub(super) fn normalize_color(v: &str) -> Option<String> {
    let t = v.trim().trim_start_matches('#');
    if t.len() == 6 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("#{}", t.to_ascii_lowercase()))
    } else {
        None
    }
}

/// Parse an OOXML twips length (`w:spacing`/`w:ind`, 1/1440 inch) to pixels at
/// 96 dpi: 1 twip = 96/1440 = 1/15 px. Mirrors the model's px-based spacing/indent
/// units (the inverse of `export::px_to_twips`). Negative values (hanging indents)
/// clamp to 0 since the model has no negative indent.
pub(super) fn twips_to_px(v: &str) -> Option<f64> {
    let n: f64 = v.trim().parse().ok()?;
    Some(((n / 15.0).max(0.0) * 100.0).round() / 100.0)
}

/// Inline marks implied by a single `w:rPr` child element.
pub(super) fn mark_from_rpr_child(e: &BytesStart) -> Option<(String, MarkValue)> {
    match e.name().as_ref() {
        b"w:b" | b"w:bCs" if toggle_on(e) => Some(("strong".into(), MarkValue::Bool(true))),
        b"w:i" | b"w:iCs" if toggle_on(e) => Some(("em".into(), MarkValue::Bool(true))),
        b"w:u" if underline_on(e) => Some(("underline".into(), MarkValue::Bool(true))),
        b"w:strike" | b"w:dstrike" if toggle_on(e) => Some(("strike".into(), MarkValue::Bool(true))),
        b"w:sz" => parse_half_point(e).map(|n| ("fontSize".into(), MarkValue::Int(n))),
        b"w:rFonts" => attr_val(e, b"w:ascii")
            .or_else(|| attr_val(e, b"w:hAnsi"))
            .map(|f| ("fontFamily".into(), MarkValue::Str(generic_font(&f)))),
        _ => None,
    }
}

#[derive(Default, Clone)]
pub(super) struct ParaStyle {
    pub parent: Option<String>,
    pub align: Option<String>,
    pub outline_level: Option<i64>,
    pub heading_name: bool,
    pub name_level: Option<i64>,
    pub text: Vec<(String, MarkValue)>,
    /// Human display name (`w:name w:val`), if any.
    pub display_name: Option<String>,
    /// A user-facing *named* paragraph style (every `w:style w:type="paragraph"`
    /// in `word/styles.xml` is one). Distinguishes real named styles the codec
    /// lifts into the CRDT styles container from styles only used for resolution.
    pub named: bool,
    /// Typed `StyleProps`-shaped fields (distinct from the inline `text` marks the
    /// same `w:rPr`/`w:pPr` imply) used to build the container style's scalars.
    pub size: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub color: Option<String>,
    pub font: Option<String>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
    pub indent: Option<f64>,
}

#[derive(Default)]
pub(super) struct StyleRegistry {
    /// `character`-family style id → inline marks.
    pub text: HashMap<String, Vec<(String, MarkValue)>>,
    /// `paragraph`-family style id → paragraph style.
    pub para: HashMap<String, ParaStyle>,
    /// `w:numId` → is the list numbered (ordered) rather than bulleted?
    pub num_ordered: HashMap<String, bool>,
}

impl StyleRegistry {
    /// Heading level for a paragraph style id, following `basedOn` chains. Falls
    /// back to recognising well-known heading ids/names (`Heading1`, …) even when
    /// the style itself isn't registered — so codec-authored exports re-import
    /// without a `styles.xml`.
    pub(super) fn heading_level(&self, name: &str) -> Option<i64> {
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let n = cur?;
            let Some(p) = self.para.get(&n) else {
                let (h, l) = heading_from_name(&n, None);
                return if h { Some(l.unwrap_or(1).clamp(1, 6)) } else { None };
            };
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

    /// Whether `id` is a user-facing *named* paragraph style (every paragraph
    /// style declared in `word/styles.xml`) — the styles lifted into the styles
    /// container. Built-in heading styles resolve to a heading level first, so the
    /// body parser never lifts them even though they are also "named".
    pub(super) fn is_named_para(&self, id: &str) -> bool {
        self.para.get(id).map(|p| p.named).unwrap_or(false)
    }

    /// The paragraph-style chain (self → basedOn → …), most-derived first.
    fn para_chain(&self, name: &str) -> Vec<&ParaStyle> {
        let mut chain = Vec::new();
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let Some(n) = cur else { break };
            let Some(p) = self.para.get(&n) else { break };
            chain.push(p);
            cur = p.parent.clone();
        }
        chain
    }

    /// Resolve a named paragraph style into the `StyleProps`-shaped scalar map the
    /// CRDT styles container stores (self overrides `basedOn` parent for each
    /// field; unset fields fall back to neutral defaults so the editor renders
    /// them, and so the round-trip is idempotent). Mirrors the `.odt` codec.
    pub(super) fn style_props(&self, id: &str) -> Option<BTreeMap<String, AttrVal>> {
        let chain = self.para_chain(id);
        if chain.is_empty() {
            return None;
        }
        // First-set-wins scan over the chain (most-derived first).
        let display = chain.iter().find_map(|p| p.display_name.clone());
        let size = chain.iter().find_map(|p| p.size).unwrap_or(16.0);
        let bold = chain.iter().find_map(|p| p.bold).unwrap_or(false);
        let italic = chain.iter().find_map(|p| p.italic).unwrap_or(false);
        let color = chain.iter().find_map(|p| p.color.clone()).unwrap_or_else(|| "#000000".to_string());
        let font = chain.iter().find_map(|p| p.font.clone()).unwrap_or_else(|| "sans".to_string());
        let align = chain.iter().find_map(|p| p.align.clone()).unwrap_or_else(|| "left".to_string());
        let space_before = chain.iter().find_map(|p| p.space_before).unwrap_or(0.0);
        let space_after = chain.iter().find_map(|p| p.space_after).unwrap_or(0.0);
        let indent = chain.iter().find_map(|p| p.indent).unwrap_or(0.0);
        let name_str = display.unwrap_or_else(|| id.to_string());

        let mut m = BTreeMap::new();
        m.insert(STYLE_NAME.to_string(), AttrVal::Str(name_str));
        m.insert(STYLE_FONT.to_string(), AttrVal::Str(font));
        m.insert(STYLE_SIZE.to_string(), AttrVal::Float(size));
        m.insert(STYLE_BOLD.to_string(), AttrVal::Bool(bold));
        m.insert(STYLE_ITALIC.to_string(), AttrVal::Bool(italic));
        m.insert(STYLE_COLOR.to_string(), AttrVal::Str(color));
        m.insert(STYLE_ALIGN.to_string(), AttrVal::Str(align));
        m.insert(STYLE_SPACE_BEFORE.to_string(), AttrVal::Float(space_before));
        m.insert(STYLE_SPACE_AFTER.to_string(), AttrVal::Float(space_after));
        m.insert(STYLE_INDENT.to_string(), AttrVal::Float(indent));
        Some(m)
    }
}

/// `(looks like a heading, level)` for a style id / display name.
fn heading_from_name(id: &str, name: Option<&str>) -> (bool, Option<i64>) {
    for c in [id, name.unwrap_or("")] {
        let lc = c.to_ascii_lowercase();
        let t = lc.trim_start();
        if t.starts_with("heading") {
            let lvl = t.split(|ch: char| !ch.is_ascii_digit()).find_map(|x| x.parse::<i64>().ok());
            return (true, lvl);
        }
    }
    (false, None)
}

/// Parse all `w:style` from a `word/styles.xml` stream into `reg`. Safe to call
/// once per import.
pub(super) fn parse_styles(xml: &[u8], reg: &mut StyleRegistry) {
    let Ok(s) = std::str::from_utf8(xml) else { return };
    let mut reader = Reader::from_str(s);

    let mut style_id: Option<String> = None;
    let mut style_type: Option<String> = None;
    let mut name_disp: Option<String> = None;
    let mut parent: Option<String> = None;
    let mut align: Option<String> = None;
    let mut outline: Option<i64> = None;
    let mut text: Vec<(String, MarkValue)> = Vec::new();
    // Typed `StyleProps`-shaped scratch (built alongside `text`).
    let mut size: Option<f64> = None;
    let mut bold: Option<bool> = None;
    let mut italic: Option<bool> = None;
    let mut color: Option<String> = None;
    let mut font: Option<String> = None;
    let mut space_before: Option<f64> = None;
    let mut space_after: Option<f64> = None;
    let mut indent: Option<f64> = None;
    let mut in_ppr = false;
    let mut in_rpr = false;

    // Apply a property element (works for both Empty and Start forms).
    macro_rules! prop {
        ($e:expr) => {{
            let e = $e;
            match e.name().as_ref() {
                b"w:name" => name_disp = attr_val(&e, b"w:val"),
                b"w:basedOn" => parent = attr_val(&e, b"w:val"),
                b"w:jc" if in_ppr => align = attr_val(&e, b"w:val").map(|v| map_jc(&v)),
                b"w:outlineLvl" if in_ppr => {
                    outline = attr_val(&e, b"w:val").and_then(|v| v.parse::<i64>().ok()).map(|n| n + 1)
                }
                // Paragraph spacing (twips) → px space_before/space_after.
                b"w:spacing" if in_ppr => {
                    if let Some(v) = attr_val(&e, b"w:before") {
                        space_before = twips_to_px(&v);
                    }
                    if let Some(v) = attr_val(&e, b"w:after") {
                        space_after = twips_to_px(&v);
                    }
                }
                // Left indent (twips, `w:left`/`w:start`) → px indent.
                b"w:ind" if in_ppr => {
                    if let Some(v) = attr_val(&e, b"w:left").or_else(|| attr_val(&e, b"w:start")) {
                        indent = twips_to_px(&v);
                    }
                }
                _ if in_rpr => {
                    // Typed style fields, in parallel with the inline `text` marks.
                    match e.name().as_ref() {
                        b"w:b" | b"w:bCs" => bold = Some(toggle_on(&e)),
                        b"w:i" | b"w:iCs" => italic = Some(toggle_on(&e)),
                        b"w:sz" => {
                            if let Some(hp) = attr_val(&e, b"w:val").and_then(|v| v.parse::<f64>().ok()) {
                                size = Some(hp / 2.0);
                            }
                        }
                        b"w:color" => {
                            if let Some(c) = attr_val(&e, b"w:val").and_then(|v| normalize_color(&v)) {
                                color = Some(c);
                            }
                        }
                        b"w:rFonts" => {
                            if let Some(f) = attr_val(&e, b"w:ascii").or_else(|| attr_val(&e, b"w:hAnsi")) {
                                font = Some(generic_font(&f));
                            }
                        }
                        _ => {}
                    }
                    if let Some((k, v)) = mark_from_rpr_child(&e) {
                        text.retain(|(k2, _)| k2 != &k);
                        text.push((k, v));
                    }
                }
                _ => {}
            }
        }};
    }

    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"w:style" => {
                    style_id = attr_val(&e, b"w:styleId");
                    style_type = attr_val(&e, b"w:type");
                    name_disp = None;
                    parent = None;
                    align = None;
                    outline = None;
                    text.clear();
                    size = None;
                    bold = None;
                    italic = None;
                    color = None;
                    font = None;
                    space_before = None;
                    space_after = None;
                    indent = None;
                    in_ppr = false;
                    in_rpr = false;
                }
                b"w:pPr" => in_ppr = true,
                b"w:rPr" => {
                    if !in_ppr {
                        in_rpr = true;
                    }
                }
                _ => prop!(e),
            },
            Event::Empty(e) => prop!(e),
            Event::End(e) => match e.name().as_ref() {
                b"w:pPr" => in_ppr = false,
                b"w:rPr" => in_rpr = false,
                b"w:style" => {
                    if let Some(id) = style_id.take() {
                        match style_type.as_deref() {
                            Some("character") => {
                                if !text.is_empty() {
                                    reg.text.insert(id, std::mem::take(&mut text));
                                }
                            }
                            Some("paragraph") => {
                                let (h, l) = heading_from_name(&id, name_disp.as_deref());
                                reg.para.insert(
                                    id,
                                    ParaStyle {
                                        parent: parent.take(),
                                        align: align.take(),
                                        outline_level: outline.take(),
                                        heading_name: h,
                                        name_level: l,
                                        text: std::mem::take(&mut text),
                                        display_name: name_disp.take(),
                                        named: true,
                                        size: size.take(),
                                        bold: bold.take(),
                                        italic: italic.take(),
                                        color: color.take(),
                                        font: font.take(),
                                        space_before: space_before.take(),
                                        space_after: space_after.take(),
                                        indent: indent.take(),
                                    },
                                );
                            }
                            _ => {}
                        }
                    }
                    style_type = None;
                    name_disp = None;
                    parent = None;
                    align = None;
                    outline = None;
                    text.clear();
                    size = None;
                    bold = None;
                    italic = None;
                    color = None;
                    font = None;
                    space_before = None;
                    space_after = None;
                    indent = None;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
}

/// Parse `word/numbering.xml` into `reg.num_ordered`: a `w:numId` is ordered iff
/// its abstract numbering's level-0 `w:numFmt` is something other than `bullet`.
pub(super) fn parse_numbering(xml: &[u8], reg: &mut StyleRegistry) {
    let Ok(s) = std::str::from_utf8(xml) else { return };
    let mut reader = Reader::from_str(s);

    let mut abstract_ordered: HashMap<String, bool> = HashMap::new();
    let mut num_to_abstract: HashMap<String, String> = HashMap::new();

    let mut cur_abstract: Option<String> = None;
    let mut cur_ilvl: Option<String> = None;
    let mut cur_num: Option<String> = None;

    macro_rules! prop {
        ($e:expr) => {{
            let e = $e;
            match e.name().as_ref() {
                b"w:lvl" => cur_ilvl = attr_val(&e, b"w:ilvl"),
                b"w:numFmt" => {
                    if cur_ilvl.as_deref() == Some("0") {
                        if let (Some(abs), Some(fmt)) = (cur_abstract.clone(), attr_val(&e, b"w:val")) {
                            abstract_ordered.insert(abs, fmt != "bullet" && fmt != "none");
                        }
                    }
                }
                b"w:abstractNumId" if cur_num.is_some() => {
                    if let (Some(num), Some(abs)) = (cur_num.clone(), attr_val(&e, b"w:val")) {
                        num_to_abstract.insert(num, abs);
                    }
                }
                _ => {}
            }
        }};
    }

    loop {
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"w:abstractNum" => {
                    cur_abstract = attr_val(&e, b"w:abstractNumId");
                    cur_ilvl = None;
                }
                b"w:num" => cur_num = attr_val(&e, b"w:numId"),
                b"w:lvl" => cur_ilvl = attr_val(&e, b"w:ilvl"),
                _ => prop!(e),
            },
            Event::Empty(e) => prop!(e),
            Event::End(e) => match e.name().as_ref() {
                b"w:abstractNum" => cur_abstract = None,
                b"w:num" => cur_num = None,
                b"w:lvl" => cur_ilvl = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }

    for (num, abs) in num_to_abstract {
        let ordered = abstract_ordered.get(&abs).copied().unwrap_or(false);
        reg.num_ordered.insert(num, ordered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_named_heading_and_character_styles() {
        let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/><w:jc w:val="center"/></w:pPr><w:rPr><w:b/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="MySub"><w:basedOn w:val="Heading1"/></w:style>
        <w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>
        </w:styles>"#;
        let mut reg = StyleRegistry::default();
        parse_styles(xml, &mut reg);
        assert_eq!(reg.heading_level("Heading1"), Some(1));
        assert_eq!(reg.heading_level("MySub"), Some(1), "inherits heading via basedOn");
        assert_eq!(reg.para_align("Heading1").as_deref(), Some("center"));
        assert!(reg.text_marks("Strong").iter().any(|(n, _)| n == "strong"));
        assert!(reg.para_text_marks("Heading1").iter().any(|(n, _)| n == "strong"));
    }

    #[test]
    fn heading_level_falls_back_to_well_known_ids() {
        // No styles.xml registered, but "Heading2" is a recognised id.
        let reg = StyleRegistry::default();
        assert_eq!(reg.heading_level("Heading2"), Some(2));
        assert_eq!(reg.heading_level("Normal"), None);
    }

    #[test]
    fn detects_ordered_vs_bulleted_numbering() {
        let xml = br#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
        <w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
        <w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
        </w:numbering>"#;
        let mut reg = StyleRegistry::default();
        parse_numbering(xml, &mut reg);
        assert_eq!(reg.num_ordered.get("1"), Some(&false), "bullet → unordered");
        assert_eq!(reg.num_ordered.get("2"), Some(&true), "decimal → ordered");
    }
}
