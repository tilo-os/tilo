//! Parse ODF styles — named styles from `styles.xml` and automatic styles from
//! `content.xml` — into a registry the body parser resolves against. Real `.odt`
//! files keep most formatting (headings, bold, fonts, list numbering) in named
//! styles, so resolving them is what makes real documents import faithfully.

use crate::document::MarkValue;
use crate::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME, STYLE_SIZE, STYLE_SPACE_AFTER,
    STYLE_SPACE_BEFORE,
};
use crate::snapshot::AttrVal;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::{BTreeMap, HashMap};

/// Decode an ODF style name into the qdoc style-id used as the container key:
/// LibreOffice encodes spaces (and other chars) as `_20_`, so `Heading_20_1`
/// becomes `Heading 1`. Inverse of [`encode_style_name`].
pub(super) fn decode_style_name(name: &str) -> String {
    name.replace("_20_", " ")
}

/// Encode a qdoc style id back into a valid ODF `style:name` (`_20_` for spaces).
pub(super) fn encode_style_name(id: &str) -> String {
    id.replace(' ', "_20_")
}

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

/// Classify an ODF `style:text-position` value as superscript (`Some(true)`),
/// subscript (`Some(false)`), or neither (`None`). The value is `<position>
/// [height]` where the position is a percent (`super`/`sub` keywords or a signed
/// percentage like `33%`/`-33%`); positive → superscript, negative → subscript.
pub(super) fn text_position_kind(v: &str) -> Option<bool> {
    let first = v.trim().split_whitespace().next()?;
    match first {
        "super" => Some(true),
        "sub" => Some(false),
        _ => {
            let pct = first.trim_end_matches('%').trim().parse::<f64>().ok()?;
            if pct > 0.0 {
                Some(true)
            } else if pct < 0.0 {
                Some(false)
            } else {
                None
            }
        }
    }
}

/// Parse an ODF `fo:line-height` percentage (e.g. `150%`) into the model's
/// `line_spacing` multiple (1.5). Only percentage line-heights map cleanly onto a
/// unitless multiple; fixed lengths (`fo:line-height="0.5cm"`) and `normal` are
/// left unset (best-effort, matching the size-percentage handling above).
pub(super) fn parse_line_height(v: &str) -> Option<f64> {
    let t = v.trim();
    let pct = t.strip_suffix('%')?.trim().parse::<f64>().ok()?;
    if pct > 0.0 {
        Some((pct / 100.0 * 1000.0).round() / 1000.0)
    } else {
        None
    }
}

pub(super) fn map_align(v: &str) -> String {
    match v {
        "center" => "center".into(),
        "end" | "right" => "right".into(),
        "justify" | "justified" => "justify".into(),
        _ => "left".into(),
    }
}

/// Map an ODF `style:wrap` value onto the model wrap (`none`|`left`|`right`|`both`).
/// `run-through`/`none` → no wrap; `parallel`/`dynamic`/`biggest` → wrap both sides.
pub(super) fn map_odf_wrap(v: &str) -> &'static str {
    match v {
        "left" => "left",
        "right" => "right",
        "parallel" | "dynamic" | "biggest" => "both",
        _ => "none", // "none" | "run-through" | unknown
    }
}

/// Map a model wrap (`none`|`left`|`right`|`both`) onto the ODF `style:wrap` value.
pub(super) fn model_wrap_to_odf(v: &str) -> &'static str {
    match v {
        "left" => "left",
        "right" => "right",
        "both" => "parallel",
        _ => "none",
    }
}

/// Parse an ODF length used in margins/indents (`fo:margin-*`, `fo:text-indent`)
/// to pixels. Mirrors `content::parse_len_px` but kept local so the styles parser
/// has no cross-module dependency.
pub(super) fn parse_margin_px(v: &str) -> Option<f64> {
    let v = v.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let n: f64 = num.trim().parse().ok()?;
    let px = match unit.trim() {
        "px" | "" => n,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "in" => n * 96.0,
        "pt" => n * 96.0 / 72.0,
        "pc" => n * 16.0,
        _ => return None,
    };
    Some((px * 100.0).round() / 100.0)
}

/// Raw style-property fields a `style:text-properties` element sets (for building
/// a named style's `StyleProps`-shaped scalars — distinct from the inline marks
/// the same element implies for text spans).
#[derive(Default, Clone)]
pub(super) struct TextFields {
    pub size: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub color: Option<String>,
    pub font: Option<String>,
}

pub(super) fn text_fields(e: &BytesStart) -> TextFields {
    let mut f = TextFields::default();
    for a in e.attributes().flatten() {
        let val = a.unescape_value().unwrap_or_default();
        match a.key.as_ref() {
            b"fo:font-weight" => f.bold = Some(val == "bold"),
            b"fo:font-style" => f.italic = Some(val == "italic"),
            // Parse as f64 (not the rounded `parse_pt`) so fractional sizes like
            // 16.5pt survive the style round-trip. Percentage sizes (`130%`) fail
            // the parse and stay unset (best-effort for real LibreOffice docs).
            b"fo:font-size" => f.size = val.trim_end_matches("pt").trim().parse::<f64>().ok(),
            b"style:font-name" | b"fo:font-family" => f.font = Some(generic_font(&val)),
            b"fo:color" => f.color = Some(normalize_color(&val)),
            _ => {}
        }
    }
    f
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
            // Text background (highlight): ODF `fo:background-color` (skip the
            // `transparent` sentinel — that's "no highlight").
            b"fo:background-color" if val != "transparent" => {
                m.push(("highlight".into(), MarkValue::Str(normalize_color(&val))))
            }
            // Vertical alignment: `style:text-position` is `<pos> <height>` where a
            // positive position (or the keyword `super`) is superscript and a
            // negative position (or `sub`) is subscript.
            b"style:text-position" => match text_position_kind(&val) {
                Some(true) => m.push(("super".into(), MarkValue::Bool(true))),
                Some(false) => m.push(("sub".into(), MarkValue::Bool(true))),
                None => {}
            },
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
    /// Human display name (`style:display-name`), if any.
    pub display_name: Option<String>,
    /// A user-facing *named* (common) style — from `styles.xml` or carrying a
    /// `style:display-name`. Distinguishes real named styles from anonymous
    /// automatic styles (`P1`, …); only named styles become container styles +
    /// explicit block `style` attrs.
    pub named: bool,
    pub size: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub color: Option<String>,
    pub font: Option<String>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
    pub indent: Option<f64>,
    /// Line spacing as a unitless multiple (e.g. 1.5), from `fo:line-height` (%).
    pub line_height: Option<f64>,
}

/// Map an ODF `style:num-format` value to the model's canonical numbering format
/// (`decimal|lower-roman|upper-roman|lower-alpha|upper-alpha`). A bullet list has
/// no num-format; unknown values fall back to `decimal`.
pub(super) fn map_num_format(v: &str) -> &'static str {
    match v.trim() {
        "i" => "lower-roman",
        "I" => "upper-roman",
        "a" => "lower-alpha",
        "A" => "upper-alpha",
        _ => "decimal", // "1" and anything else
    }
}

/// The captured numbering of one `text:list-style` level (level 0 is what the
/// model preserves): its canonical format + start value.
#[derive(Default)]
pub(super) struct ListNumbering {
    pub num_format: Option<&'static str>,
    pub num_start: Option<i64>,
}

#[derive(Default)]
pub(super) struct StyleRegistry {
    /// `text`-family style name → inline marks.
    pub text: HashMap<String, Vec<(String, MarkValue)>>,
    /// `paragraph`-family style name → paragraph style.
    pub para: HashMap<String, ParaStyle>,
    /// list-style name → is it numbered (ordered)?
    pub list_ordered: HashMap<String, bool>,
    /// list-style name → its level-0 numbering (format + start), for fidelity
    /// preservation of ordered lists.
    pub list_numbering: HashMap<String, ListNumbering>,
    /// `graphic`-family (frame) style name → the model wrap value
    /// (`none`|`left`|`right`|`both`) parsed from its `style:wrap`.
    pub graphic_wrap: HashMap<String, String>,
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

    /// Line spacing (`fo:line-height` %) a paragraph style applies, following the
    /// parent chain (most-derived first).
    pub(super) fn para_line_height(&self, name: &str) -> Option<f64> {
        let mut cur = Some(name.to_string());
        for _ in 0..8 {
            let n = cur?;
            let p = self.para.get(&n)?;
            if let Some(lh) = p.line_height {
                return Some(lh);
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

    /// Whether `name` is a user-facing *named* paragraph style (vs an anonymous
    /// automatic style) — the styles we lift into the CRDT styles container.
    pub(super) fn is_named_para(&self, name: &str) -> bool {
        self.para.get(name).map(|p| p.named).unwrap_or(false)
    }

    /// The paragraph-style chain (self → parent → …), most-derived first.
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
    /// CRDT styles container stores (self overrides parent for each field;
    /// unset fields fall back to neutral defaults so the editor renders them).
    pub(super) fn style_props(&self, name: &str) -> Option<BTreeMap<String, AttrVal>> {
        let chain = self.para_chain(name);
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
        let name_str = display.unwrap_or_else(|| decode_style_name(name));

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
/// `from_named_section` is `true` for `styles.xml` (common/named styles) so those
/// styles are flagged as user-facing named styles even without a display-name.
pub(super) fn parse_styles(xml: &[u8], reg: &mut StyleRegistry, from_named_section: bool) {
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
    let mut display: Option<String> = None;
    let mut tf = TextFields::default();
    let mut space_before: Option<f64> = None;
    let mut space_after: Option<f64> = None;
    let mut indent: Option<f64> = None;
    let mut line_height: Option<f64> = None;
    // Frame (graphic-family) wrap, parsed from `style:graphic-properties style:wrap`.
    let mut graphic_wrap: Option<String> = None;

    let mut list_name: Option<String> = None;
    let mut list_numbered = false;
    // Level-0 numbering captured from the current list-style (format + start).
    let mut list_num_format: Option<&'static str> = None;
    let mut list_num_start: Option<i64> = None;

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
                        let disp = attr_val(&e, b"style:display-name");
                        let (h, l) = heading_from_name(&nm, disp.as_deref());
                        reg.para.insert(
                            nm,
                            ParaStyle {
                                parent: attr_val(&e, b"style:parent-style-name"),
                                outline_level: attr_val(&e, b"style:default-outline-level").and_then(|v| v.parse().ok()),
                                heading_name: h,
                                name_level: l,
                                named: from_named_section || disp.is_some(),
                                display_name: disp,
                                ..Default::default()
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
                    display = attr_val(&e, b"style:display-name");
                    let (h, l) = heading_from_name(name.as_deref().unwrap_or(""), display.as_deref());
                    heading_name = h;
                    name_level = l;
                    align = None;
                    text = Vec::new();
                    tf = TextFields::default();
                    space_before = None;
                    space_after = None;
                    indent = None;
                    line_height = None;
                    graphic_wrap = None;
                }
                b"style:graphic-properties" => {
                    if let Some(w) = attr_val(&e, b"style:wrap") {
                        graphic_wrap = Some(map_odf_wrap(&w).to_string());
                    }
                }
                b"style:text-properties" => {
                    text.extend(marks_from_text_props(&e));
                    let f = text_fields(&e);
                    if f.size.is_some() {
                        tf.size = f.size;
                    }
                    if f.bold.is_some() {
                        tf.bold = f.bold;
                    }
                    if f.italic.is_some() {
                        tf.italic = f.italic;
                    }
                    if f.color.is_some() {
                        tf.color = f.color;
                    }
                    if f.font.is_some() {
                        tf.font = f.font;
                    }
                }
                b"style:paragraph-properties" => {
                    if let Some(a) = attr_val(&e, b"fo:text-align") {
                        align = Some(map_align(&a));
                    }
                    if let Some(v) = attr_val(&e, b"fo:margin-top") {
                        space_before = parse_margin_px(&v);
                    }
                    if let Some(v) = attr_val(&e, b"fo:margin-bottom") {
                        space_after = parse_margin_px(&v);
                    }
                    // Left indent: prefer fo:margin-left, fall back to text-indent.
                    if let Some(v) = attr_val(&e, b"fo:margin-left").or_else(|| attr_val(&e, b"fo:text-indent")) {
                        indent = parse_margin_px(&v);
                    }
                    if let Some(v) = attr_val(&e, b"fo:line-height") {
                        line_height = parse_line_height(&v);
                    }
                }
                b"text:list-style" => {
                    list_name = attr_val(&e, b"style:name");
                    list_numbered = false;
                    list_num_format = None;
                    list_num_start = None;
                }
                b"text:list-level-style-number" => {
                    list_numbered = true;
                    // Capture the level-1 (0-based level 0) format + start. ODF
                    // numbers levels from 1; `text:level="1"` is the model's level 0.
                    let is_lvl0 = attr_val(&e, b"text:level").map(|l| l == "1").unwrap_or(true);
                    if is_lvl0 {
                        if let Some(f) = attr_val(&e, b"style:num-format") {
                            list_num_format = Some(map_num_format(&f));
                        }
                        if let Some(s) = attr_val(&e, b"text:start-value").and_then(|v| v.parse::<i64>().ok()) {
                            list_num_start = Some(s);
                        }
                    }
                }
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
                                        named: from_named_section || display.is_some(),
                                        display_name: display.take(),
                                        size: tf.size.take(),
                                        bold: tf.bold.take(),
                                        italic: tf.italic.take(),
                                        color: tf.color.take(),
                                        font: tf.font.take(),
                                        space_before: space_before.take(),
                                        space_after: space_after.take(),
                                        indent: indent.take(),
                                        line_height: line_height.take(),
                                    },
                                );
                            }
                            "graphic" => {
                                if let Some(w) = graphic_wrap.take() {
                                    reg.graphic_wrap.insert(n, w);
                                }
                            }
                            _ => {}
                        }
                    }
                    parent = None;
                    outline = None;
                    heading_name = false;
                    display = None;
                    text.clear();
                    tf = TextFields::default();
                    space_before = None;
                    space_after = None;
                    indent = None;
                    line_height = None;
                    graphic_wrap = None;
                }
                b"text:list-style" => {
                    if let Some(n) = list_name.take() {
                        reg.list_ordered.insert(n.clone(), list_numbered);
                        reg.list_numbering.insert(
                            n,
                            ListNumbering { num_format: list_num_format.take(), num_start: list_num_start.take() },
                        );
                    }
                    list_num_format = None;
                    list_num_start = None;
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
        parse_styles(xml, &mut reg, true);
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
        parse_styles(xml, &mut reg, false);
        assert_eq!(reg.list_ordered.get("L1"), Some(&true));
        assert_eq!(reg.list_ordered.get("L2"), Some(&false));
    }
}
