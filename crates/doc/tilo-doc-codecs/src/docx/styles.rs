//! Parse OOXML styles (`word/styles.xml`) and list numbering (`word/numbering.xml`) into
//! a registry the body parser resolves against — `.docx` files keep most formatting
//! in named styles and list ordering in numbering definitions.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::{BTreeMap, HashMap};
use tilo_doc::document::MarkValue;
use tilo_doc::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME,
    STYLE_SIZE, STYLE_SPACE_AFTER, STYLE_SPACE_BEFORE,
};
use tilo_doc::snapshot::AttrVal;

pub(super) fn attr_val(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| crate::unescape_attr(&a))
}

/// OOXML toggle property (`<w:b/>`, `<w:i/>`, …): present means *on* unless `w:val` explicitly turns it off.
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
#[allow(
    clippy::cast_possible_truncation,
    reason = "a font half-point size from OOXML's own bounded w:val range; far below i64::MAX"
)]
pub(super) fn parse_half_point(e: &BytesStart) -> Option<i64> {
    attr_val(e, b"w:val")
        .and_then(|v| v.parse::<f64>().ok())
        .map(|hp| (hp / 2.0).round() as i64)
}

pub(super) fn generic_font(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    if n.contains("mono") || n.contains("courier") || n.contains("consol") {
        "mono".into()
    } else if (n.contains("serif") && !n.contains("sans"))
        || n.contains("times")
        || n.contains("georgia")
        || n.contains("garamond")
        || n.contains("roman")
    {
        "serif".into()
    } else {
        "sans".into()
    }
}

pub(super) fn map_jc(v: &str) -> String {
    match v {
        "center" => "center".into(),
        "end" | "right" => "right".into(),
        "both" | "distribute" | "justify" => "justify".into(),
        _ => "left".into(),
    }
}

/// Normalise an OOXML `w:color w:val` to the model's canonical `#rrggbb`; `auto`/non-hex values yield `None`.
pub(super) fn normalize_color(v: &str) -> Option<String> {
    let t = v.trim().trim_start_matches('#');
    if t.len() == 6 && t.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("#{}", t.to_ascii_lowercase()))
    } else {
        None
    }
}

/// OOXML twips (1/1440 inch) to pixels at 96 dpi (1 twip = 1/15 px), the inverse of `export::px_to_twips`.
pub(super) fn twips_to_px(v: &str) -> Option<f64> {
    let n: f64 = v.trim().parse().ok()?;
    Some(((n / 15.0).max(0.0) * 100.0).round() / 100.0)
}

/// Line spacing from `<w:spacing>`: an `auto`-rule `w:line` is in 240ths of a line (1.5 → 360); other rules are left unset.
pub(super) fn line_spacing_from_spacing(e: &BytesStart) -> Option<f64> {
    let line = attr_val(e, b"w:line").and_then(|v| v.trim().parse::<f64>().ok())?;
    let rule = attr_val(e, b"w:lineRule").unwrap_or_else(|| "auto".to_string());
    if rule == "auto" && line > 0.0 {
        Some((line / 240.0 * 1000.0).round() / 1000.0)
    } else {
        None
    }
}

pub(super) fn mark_from_rpr_child(e: &BytesStart) -> Option<(String, MarkValue)> {
    match e.name().as_ref() {
        b"w:b" | b"w:bCs" if toggle_on(e) => Some(("strong".into(), MarkValue::Bool(true))),
        b"w:i" | b"w:iCs" if toggle_on(e) => Some(("em".into(), MarkValue::Bool(true))),
        b"w:u" if underline_on(e) => Some(("underline".into(), MarkValue::Bool(true))),
        b"w:strike" | b"w:dstrike" if toggle_on(e) => {
            Some(("strike".into(), MarkValue::Bool(true)))
        }
        b"w:sz" => parse_half_point(e).map(|n| ("fontSize".into(), MarkValue::Int(n))),
        b"w:rFonts" => attr_val(e, b"w:ascii")
            .or_else(|| attr_val(e, b"w:hAnsi"))
            .map(|f| ("fontFamily".into(), MarkValue::Str(generic_font(&f)))),
        b"w:color" => attr_val(e, b"w:val")
            .and_then(|v| normalize_color(&v))
            .map(|c| ("color".into(), MarkValue::Str(c))),
        // `w:lang w:val` is already a BCP-47 tag, so it passes straight through to a `lang` mark.
        b"w:lang" => attr_val(e, b"w:val")
            .filter(|v| !v.is_empty())
            .map(|v| ("lang".into(), MarkValue::Str(v))),
        // Prefer `w:shd w:fill` (arbitrary hex); `w:highlight` carries a named colour mapped to hex.
        b"w:shd" => attr_val(e, b"w:fill")
            .filter(|v| !v.eq_ignore_ascii_case("auto"))
            .and_then(|v| normalize_color(&v))
            .map(|c| ("highlight".into(), MarkValue::Str(c))),
        b"w:highlight" => attr_val(e, b"w:val")
            .filter(|v| !v.eq_ignore_ascii_case("none"))
            .and_then(|v| highlight_name_to_hex(&v))
            .map(|c| ("highlight".into(), MarkValue::Str(c))),
        // Vertical alignment: `w:vertAlign w:val="superscript"|"subscript"`.
        b"w:vertAlign" => match attr_val(e, b"w:val").as_deref() {
            Some("superscript") => Some(("super".into(), MarkValue::Bool(true))),
            Some("subscript") => Some(("sub".into(), MarkValue::Bool(true))),
            _ => None,
        },
        _ => None,
    }
}

/// The document default language from `<w:docDefaults><w:rPrDefault><w:rPr><w:lang>`; a `<w:style>`'s own `<w:lang>` is that style's language, not read here.
pub(super) fn doc_default_language(styles_xml: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(styles_xml).ok()?;
    let mut reader = Reader::from_str(s);
    // Track the `docDefaults > rPrDefault > rPr` subtree; a `<w:lang>` inside a style's rPr is that style's language, not the default.
    let (mut in_doc_defaults, mut in_rpr_default, mut in_rpr) = (false, false, false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"w:docDefaults" => in_doc_defaults = true,
                b"w:rPrDefault" if in_doc_defaults => in_rpr_default = true,
                b"w:rPr" if in_rpr_default => in_rpr = true,
                _ => {}
            },
            Ok(Event::Empty(e)) if in_rpr && e.name().as_ref() == b"w:lang" => {
                if let Some(v) = attr_val(&e, b"w:val").filter(|v| !v.is_empty()) {
                    return Some(v);
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:rPr" => in_rpr = false,
                b"w:rPrDefault" => in_rpr_default = false,
                b"w:docDefaults" => return None, // default block closed without a lang
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
}

/// Map an OOXML `w:highlight` named colour to `#rrggbb`; the palette is a fixed 16 names.
pub(super) fn highlight_name_to_hex(name: &str) -> Option<String> {
    let hex = match name.trim().to_ascii_lowercase().as_str() {
        "black" => "#000000",
        "blue" => "#0000ff",
        "cyan" => "#00ffff",
        "green" => "#008000",
        "magenta" => "#ff00ff",
        "red" => "#ff0000",
        "yellow" => "#ffff00",
        "white" => "#ffffff",
        "darkblue" => "#000080",
        "darkcyan" => "#008080",
        "darkgreen" => "#003300",
        "darkmagenta" => "#800080",
        "darkred" => "#800000",
        "darkyellow" => "#808000",
        "darkgray" => "#808080",
        "lightgray" => "#c0c0c0",
        _ => return None,
    };
    Some(hex.to_string())
}

#[derive(Default, Clone)]
pub(crate) struct ParaStyle {
    pub parent: Option<String>,
    pub align: Option<String>,
    pub outline_level: Option<i64>,
    pub heading_name: bool,
    pub name_level: Option<i64>,
    pub text: Vec<(String, MarkValue)>,
    pub display_name: Option<String>,
    /// A user-facing *named* paragraph style, distinguishing real named styles lifted into the styles container from resolution-only ones.
    pub named: bool,
    /// Typed `StyleProps`-shaped fields (distinct from the inline `text` marks the same `w:rPr`/`w:pPr` imply).
    pub size: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub color: Option<String>,
    pub font: Option<String>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
    pub indent: Option<f64>,
    /// Line spacing as a unitless multiple (1.5), from `w:spacing w:line` (auto).
    pub line_spacing: Option<f64>,
}

/// Map an OOXML `w:numFmt w:val` onto the model's numbering format; unknown ordered formats fall back to `decimal`.
pub(super) fn map_num_format(v: &str) -> Option<&'static str> {
    match v {
        "bullet" | "none" => None,
        "lowerRoman" => Some("lower-roman"),
        "upperRoman" => Some("upper-roman"),
        "lowerLetter" => Some("lower-alpha"),
        "upperLetter" => Some("upper-alpha"),
        _ => Some("decimal"), // "decimal" and other ordered formats
    }
}

/// The captured level-0 numbering of a `w:numId`, for fidelity preservation of ordered lists.
#[derive(Clone, Default)]
pub(crate) struct NumDef {
    pub ordered: bool,
    pub num_format: Option<&'static str>,
    pub num_start: Option<i64>,
}

#[derive(Default)]
pub(crate) struct StyleRegistry {
    /// `character`-family style id → inline marks.
    pub text: HashMap<String, Vec<(String, MarkValue)>>,
    /// `paragraph`-family style id → paragraph style.
    pub para: HashMap<String, ParaStyle>,
    /// `w:numId` → is the list numbered (ordered) rather than bulleted?
    pub num_ordered: HashMap<String, bool>,
    /// `w:numId` → its level-0 numbering (format + start) for fidelity.
    pub num_def: HashMap<String, NumDef>,
}

impl StyleRegistry {
    /// Heading level for a paragraph style id, following `basedOn` chains. Falls back to well-known heading ids/names even
    /// when unregistered, so codec-authored exports re-import without a `styles.xml`.
    pub(super) fn heading_level(&self, name: &str) -> Option<i64> {
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let n = cur.take()?;
            let Some(p) = self.para.get(&n) else {
                let (h, l) = heading_from_name(&n, None);
                return if h {
                    Some(l.unwrap_or(1).clamp(1, 6))
                } else {
                    None
                };
            };
            if let Some(l) = p.outline_level {
                return Some(l.clamp(1, 6));
            }
            if p.heading_name {
                return Some(p.name_level.unwrap_or(1).clamp(1, 6));
            }
            cur.clone_from(&p.parent);
        }
        None
    }

    pub(super) fn para_align(&self, name: &str) -> Option<String> {
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let n = cur.take()?;
            let p = self.para.get(&n)?;
            if let Some(a) = &p.align {
                return Some(a.clone());
            }
            cur.clone_from(&p.parent);
        }
        None
    }

    /// Line spacing (`w:spacing w:line`, auto) a paragraph style applies, following the `basedOn` chain (most-derived first).
    pub(super) fn para_line_spacing(&self, name: &str) -> Option<f64> {
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let n = cur.take()?;
            let p = self.para.get(&n)?;
            if let Some(ls) = p.line_spacing {
                return Some(ls);
            }
            cur.clone_from(&p.parent);
        }
        None
    }

    /// Inline marks a paragraph style applies to its whole text (parent-first, most-derived wins).
    pub(super) fn para_text_marks(&self, name: &str) -> Vec<(String, MarkValue)> {
        let mut chain: Vec<&ParaStyle> = Vec::new();
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let Some(n) = cur.take() else { break };
            let Some(p) = self.para.get(&n) else { break };
            chain.push(p);
            cur.clone_from(&p.parent);
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

    /// Whether `id` is a user-facing *named* paragraph style. Built-in heading styles resolve to a heading level first, so
    /// the body parser never lifts them even though they are also "named".
    pub(super) fn is_named_para(&self, id: &str) -> bool {
        self.para.get(id).is_some_and(|p| p.named)
    }

    /// The paragraph-style chain (self → basedOn → …), most-derived first.
    fn para_chain(&self, name: &str) -> Vec<&ParaStyle> {
        let mut chain = Vec::new();
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let Some(n) = cur.take() else { break };
            let Some(p) = self.para.get(&n) else { break };
            chain.push(p);
            cur.clone_from(&p.parent);
        }
        chain
    }

    /// Resolve a named paragraph style into the scalar map the CRDT styles container stores (self overrides `basedOn`
    /// parent per field; unset fields fall back to neutral defaults, keeping the round-trip idempotent).
    pub(super) fn style_props(&self, id: &str) -> Option<BTreeMap<String, AttrVal>> {
        let chain = self.para_chain(id);
        if chain.is_empty() {
            return None;
        }
        let display = chain.iter().find_map(|p| p.display_name.clone());
        let size = chain.iter().find_map(|p| p.size).unwrap_or(16.0);
        let bold = chain.iter().find_map(|p| p.bold).unwrap_or(false);
        let italic = chain.iter().find_map(|p| p.italic).unwrap_or(false);
        let color = chain
            .iter()
            .find_map(|p| p.color.clone())
            .unwrap_or_else(|| "#000000".to_string());
        let font = chain
            .iter()
            .find_map(|p| p.font.clone())
            .unwrap_or_else(|| "sans".to_string());
        let align = chain
            .iter()
            .find_map(|p| p.align.clone())
            .unwrap_or_else(|| "left".to_string());
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
            let lvl = t
                .split(|ch: char| !ch.is_ascii_digit())
                .find_map(|x| x.parse::<i64>().ok());
            return (true, lvl);
        }
    }
    (false, None)
}

/// Properties collected for the `<w:style>` being parsed.
#[derive(Default)]
struct StyleAccum {
    id: Option<String>,
    /// `w:type`: `paragraph`, `character`, ….
    kind: Option<String>,
    display_name: Option<String>,
    parent: Option<String>,
    align: Option<String>,
    outline: Option<i64>,
    text: Vec<(String, MarkValue)>,
    size: Option<f64>,
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<String>,
    font: Option<String>,
    space_before: Option<f64>,
    space_after: Option<f64>,
    indent: Option<f64>,
    line_spacing: Option<f64>,
}

impl StyleAccum {
    /// A fresh accumulator for `<w:style>` element `e`.
    fn begin(e: &BytesStart) -> Self {
        Self {
            id: attr_val(e, b"w:styleId"),
            kind: attr_val(e, b"w:type"),
            ..Self::default()
        }
    }

    /// Apply one property element (works for both Empty and Start forms).
    #[allow(
        clippy::similar_names,
        reason = "in_ppr/in_rpr mirror the OOXML element names w:pPr (paragraph properties) and w:rPr (run properties) verbatim"
    )]
    fn apply(&mut self, e: &BytesStart, in_ppr: bool, in_rpr: bool) {
        match e.name().as_ref() {
            b"w:name" => self.display_name = attr_val(e, b"w:val"),
            b"w:basedOn" => self.parent = attr_val(e, b"w:val"),
            b"w:jc" if in_ppr => self.align = attr_val(e, b"w:val").map(|v| map_jc(&v)),
            b"w:outlineLvl" if in_ppr => {
                self.outline = attr_val(e, b"w:val")
                    .and_then(|v| v.parse::<i64>().ok())
                    .map(|n| n.saturating_add(1));
            }
            // Paragraph spacing (twips) → px space_before/space_after; an auto-rule `w:line` → the unitless line_spacing multiple.
            b"w:spacing" if in_ppr => {
                if let Some(v) = attr_val(e, b"w:before") {
                    self.space_before = twips_to_px(&v);
                }
                if let Some(v) = attr_val(e, b"w:after") {
                    self.space_after = twips_to_px(&v);
                }
                if let Some(ls) = line_spacing_from_spacing(e) {
                    self.line_spacing = Some(ls);
                }
            }
            // Left indent (twips, `w:left`/`w:start`) → px indent.
            b"w:ind" if in_ppr => {
                if let Some(v) = attr_val(e, b"w:left").or_else(|| attr_val(e, b"w:start")) {
                    self.indent = twips_to_px(&v);
                }
            }
            _ if in_rpr => {
                match e.name().as_ref() {
                    b"w:b" | b"w:bCs" => self.bold = Some(toggle_on(e)),
                    b"w:i" | b"w:iCs" => self.italic = Some(toggle_on(e)),
                    b"w:sz" => {
                        if let Some(hp) = attr_val(e, b"w:val").and_then(|v| v.parse::<f64>().ok())
                        {
                            self.size = Some(hp / 2.0);
                        }
                    }
                    b"w:color" => {
                        if let Some(c) = attr_val(e, b"w:val").and_then(|v| normalize_color(&v)) {
                            self.color = Some(c);
                        }
                    }
                    b"w:rFonts" => {
                        if let Some(f) = attr_val(e, b"w:ascii").or_else(|| attr_val(e, b"w:hAnsi"))
                        {
                            self.font = Some(generic_font(&f));
                        }
                    }
                    _ => {}
                }
                if let Some((k, v)) = mark_from_rpr_child(e) {
                    self.text.retain(|(k2, _)| k2 != &k);
                    self.text.push((k, v));
                }
            }
            _ => {}
        }
    }

    /// On `</w:style>`: if a style id was captured, register it (`character` → text marks by
    /// id, `paragraph` → a resolved [`ParaStyle`]).
    fn finish(self, reg: &mut StyleRegistry) {
        let Some(id) = self.id else {
            return;
        };
        match self.kind.as_deref() {
            Some("character") => {
                if !self.text.is_empty() {
                    reg.text.insert(id, self.text);
                }
            }
            Some("paragraph") => {
                let (h, l) = heading_from_name(&id, self.display_name.as_deref());
                reg.para.insert(
                    id,
                    ParaStyle {
                        parent: self.parent,
                        align: self.align,
                        outline_level: self.outline,
                        heading_name: h,
                        name_level: l,
                        text: self.text,
                        display_name: self.display_name,
                        named: true,
                        size: self.size,
                        bold: self.bold,
                        italic: self.italic,
                        color: self.color,
                        font: self.font,
                        space_before: self.space_before,
                        space_after: self.space_after,
                        indent: self.indent,
                        line_spacing: self.line_spacing,
                    },
                );
            }
            _ => {}
        }
    }
}

#[allow(
    clippy::similar_names,
    reason = "in_ppr/in_rpr mirror the OOXML element names w:pPr (paragraph properties) and w:rPr (run properties) verbatim"
)]
pub(super) fn parse_styles(xml: &[u8], reg: &mut StyleRegistry) {
    let Ok(s) = std::str::from_utf8(xml) else {
        return;
    };
    let mut reader = Reader::from_str(s);

    let mut style = StyleAccum::default();
    let mut in_ppr = false;
    let mut in_rpr = false;

    while let Ok(ev) = reader.read_event() {
        match ev {
            Event::Start(e) => match e.name().as_ref() {
                b"w:style" => {
                    style = StyleAccum::begin(&e);
                    in_ppr = false;
                    in_rpr = false;
                }
                b"w:pPr" => in_ppr = true,
                b"w:rPr" => {
                    if !in_ppr {
                        in_rpr = true;
                    }
                }
                _ => style.apply(&e, in_ppr, in_rpr),
            },
            Event::Empty(e) => style.apply(&e, in_ppr, in_rpr),
            Event::End(e) => match e.name().as_ref() {
                b"w:pPr" => in_ppr = false,
                b"w:rPr" => in_rpr = false,
                b"w:style" => std::mem::take(&mut style).finish(reg),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
}

/// Parse `word/numbering.xml` into `reg.num_ordered`: ordered iff the abstract numbering's level-0 `w:numFmt` isn't `bullet`.
pub(super) fn parse_numbering(xml: &[u8], reg: &mut StyleRegistry) {
    let Ok(s) = std::str::from_utf8(xml) else {
        return;
    };
    let mut reader = Reader::from_str(s);

    // abstractNumId → its level-0 numbering, and which numId maps to which abstractNum.
    let mut abstract_def: HashMap<String, NumDef> = HashMap::new();
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
                        if let (Some(abs), Some(fmt)) =
                            (cur_abstract.clone(), attr_val(&e, b"w:val"))
                        {
                            let entry = abstract_def.entry(abs).or_default();
                            entry.num_format = map_num_format(&fmt);
                            entry.ordered = entry.num_format.is_some();
                        }
                    }
                }
                b"w:start" => {
                    if cur_ilvl.as_deref() == Some("0") {
                        if let (Some(abs), Some(st)) = (
                            cur_abstract.clone(),
                            attr_val(&e, b"w:val").and_then(|v| v.parse::<i64>().ok()),
                        ) {
                            abstract_def.entry(abs).or_default().num_start = Some(st);
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

    while let Ok(ev) = reader.read_event() {
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
        let def = abstract_def.get(&abs).cloned().unwrap_or_default();
        reg.num_ordered.insert(num.clone(), def.ordered);
        reg.num_def.insert(num, def);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_and_numbering_resolve() {
        // Phase `resolves_named_heading_and_character_styles`.
        {
            let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/><w:jc w:val="center"/></w:pPr><w:rPr><w:b/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="MySub"><w:basedOn w:val="Heading1"/></w:style>
            <w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>
            </w:styles>"#;
            let mut reg = StyleRegistry::default();
            parse_styles(xml, &mut reg);
            assert_eq!(reg.heading_level("Heading1"), Some(1));
            assert_eq!(
                reg.heading_level("MySub"),
                Some(1),
                "inherits heading via basedOn"
            );
            assert_eq!(reg.para_align("Heading1").as_deref(), Some("center"));
            assert!(reg.text_marks("Strong").iter().any(|(n, _)| n == "strong"));
            assert!(
                reg.para_text_marks("Heading1")
                    .iter()
                    .any(|(n, _)| n == "strong")
            );
        }
        // Phase `heading_level_falls_back_to_well_known_ids`.
        {
            // No styles.xml registered, but "Heading2" is a recognised id.
            let reg = StyleRegistry::default();
            assert_eq!(reg.heading_level("Heading2"), Some(2));
            assert_eq!(reg.heading_level("Normal"), None);
        }
        // Phase `detects_ordered_vs_bulleted_numbering`.
        {
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
}
