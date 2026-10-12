//! Parse ODF styles — named styles from `styles.xml` and automatic styles from
//! `content.xml` — into a registry the body parser resolves against. `.odt`
//! files keep most formatting (headings, bold, fonts, list numbering) in named
//! styles, which are resolved before body content is imported.

use crate::para_styles::{ParaStyle, StyleTable};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::{HashMap, HashSet};
use tilo_doc::document::MarkValue;

/// Decode an ODF style name into the qdoc style-id used as the container key:
/// `LibreOffice` encodes spaces (and other chars) as `_20_`, so `Heading_20_1` becomes `Heading 1`.
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
        .and_then(|a| crate::unescape_attr(&a))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a font point size from ODF's own bounded style attribute; far below i64::MAX"
)]
pub(super) fn parse_pt(v: &str) -> Option<i64> {
    v.trim_end_matches("pt")
        .trim()
        .parse::<f64>()
        .ok()
        .map(|f| f.round() as i64)
}

/// Canonicalise an ODF `fo:color` (`#RRGGBB`) to lowercase so that the round-trip
/// is stable and two replicas that imported the same colour converge.
pub(super) fn normalize_color(v: &str) -> String {
    let t = v.trim();
    if let Some(hex) = t.strip_prefix('#')
        && hex.len() == 6
        && hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        return format!("#{}", hex.to_ascii_lowercase());
    }
    t.to_string()
}

/// Classify an ODF `style:text-position` (`<pos> [height]`) as superscript (`Some(true)`),
/// subscript (`Some(false)`), or neither: positive/`super` → superscript, negative/`sub` → subscript.
pub(super) fn text_position_kind(v: &str) -> Option<bool> {
    let first = v.split_whitespace().next()?;
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

/// Parse an ODF `fo:line-height` percentage into the model's unitless `line_spacing`
/// multiple; fixed lengths and `normal` are left unset (best-effort).
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

/// Parse an ODF length used in margins/indents (`fo:margin-*`, `fo:text-indent`) to pixels.
/// Mirrors `content::parse_len_px` but kept local to avoid a cross-module dependency.
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

/// Raw style-property fields a `style:text-properties` element sets, for a named style's
/// `StyleProps` scalars — distinct from the inline marks the same element implies for spans.
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
        let val = crate::unescape_attr(&a).unwrap_or_default();
        match a.key.as_ref() {
            b"fo:font-weight" => f.bold = Some(val == "bold"),
            b"fo:font-style" => f.italic = Some(val == "italic"),
            // Parse as f64 (not the rounded `parse_pt`) so fractional sizes like 16.5pt survive the
            // round-trip; percentage sizes (`130%`) fail the parse and stay unset.
            b"fo:font-size" => f.size = val.trim_end_matches("pt").trim().parse::<f64>().ok(),
            b"style:font-name" | b"fo:font-family" => {
                f.font = Some(crate::generic_font_family(&val).to_owned());
            }
            b"fo:color" => f.color = Some(normalize_color(&val)),
            _ => {}
        }
    }
    f
}

/// Reassemble a BCP-47 tag from `fo:language` + optional `fo:country` (`de`+`DE` → `de-DE`).
/// `None` when no language part is present (ODF's `none` sentinel also yields `None`).
pub(super) fn reassemble_bcp47(language: Option<&str>, country: Option<&str>) -> Option<String> {
    let lang = language
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "none")?;
    match country
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "none")
    {
        Some(c) => Some(format!("{lang}-{c}")),
        None => Some(lang.to_string()),
    }
}

/// Inline marks (with values) implied by a `style:text-properties` element.
pub(super) fn marks_from_text_props(e: &BytesStart) -> Vec<(String, MarkValue)> {
    let mut m = Vec::new();
    // Per-run language override is split across fo:language/fo:country; collect both, then reassemble one BCP-47 `lang` mark.
    let (mut fo_language, mut fo_country) = (None, None);
    for a in e.attributes().flatten() {
        let val = crate::unescape_attr(&a).unwrap_or_default();
        match a.key.as_ref() {
            b"fo:font-weight" if val == "bold" => m.push(("strong".into(), MarkValue::Bool(true))),
            b"fo:font-style" if val == "italic" => m.push(("em".into(), MarkValue::Bool(true))),
            b"style:text-underline-style" if val != "none" => {
                m.push(("underline".into(), MarkValue::Bool(true)));
            }
            b"style:text-line-through-style" if val != "none" => {
                m.push(("strike".into(), MarkValue::Bool(true)));
            }
            b"fo:font-size" => {
                if let Some(n) = parse_pt(&val) {
                    m.push(("fontSize".into(), MarkValue::Int(n)));
                }
            }
            b"style:font-name" | b"fo:font-family" => {
                m.push((
                    "fontFamily".into(),
                    MarkValue::Str(crate::generic_font_family(&val).to_owned()),
                ));
            }
            b"fo:color" => m.push(("color".into(), MarkValue::Str(normalize_color(&val)))),
            // Text background (highlight): fo:background-color, skipping the `transparent` sentinel ("no highlight").
            b"fo:background-color" if val != "transparent" => {
                m.push(("highlight".into(), MarkValue::Str(normalize_color(&val))));
            }
            // Vertical alignment: style:text-position is `<pos> <height>`; a positive position (or `super`) is superscript, negative (or `sub`) is subscript.
            b"style:text-position" => match text_position_kind(&val) {
                Some(true) => m.push(("super".into(), MarkValue::Bool(true))),
                Some(false) => m.push(("sub".into(), MarkValue::Bool(true))),
                None => {}
            },
            // Per-run language override: fo:language (+ optional fo:country) reassembled into one BCP-47 `lang` mark below.
            b"fo:language" => fo_language = Some(val),
            b"fo:country" => fo_country = Some(val),
            _ => {}
        }
    }
    if let Some(code) = reassemble_bcp47(fo_language.as_deref(), fo_country.as_deref()) {
        m.push(("lang".into(), MarkValue::Str(code)));
    }
    m
}

/// Map an ODF `style:num-format` value to the model's canonical numbering format. A bullet
/// list has no num-format; unknown values fall back to `decimal`.
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
pub(crate) struct ListNumbering {
    pub num_format: Option<&'static str>,
    pub num_start: Option<i64>,
}

#[derive(Default)]
pub(crate) struct StyleRegistry {
    /// `text`- and `paragraph`-family styles by name.
    pub table: StyleTable,
    /// list-style name → is it numbered (ordered)?
    pub list_ordered: HashMap<String, bool>,
    /// list-style name → its level-0 numbering (format + start), for ordered-list fidelity.
    pub list_numbering: HashMap<String, ListNumbering>,
    /// `graphic`-family (frame) style name → the model wrap value, parsed from `style:wrap`.
    pub graphic_wrap: HashMap<String, String>,
    /// `text`-family styles that say more than their marks: named (character) styles, styles
    /// derived from one, and styles with text properties the model has no mark for.
    pub text_extra: HashSet<String>,
}

fn heading_from_name(name: &str, display: Option<&str>) -> (bool, Option<i64>) {
    for c in [name, display.unwrap_or("")] {
        let c = c.replace("_20_", " ");
        if c.to_ascii_lowercase().trim_start().starts_with("heading") {
            let lvl = c
                .split(|ch: char| !ch.is_ascii_digit())
                .find_map(|t| t.parse::<i64>().ok());
            return (true, lvl);
        }
    }
    (false, None)
}

/// Parse all `style:style`/`text:list-style` from an ODF xml stream into `reg`. Safe to call
/// repeatedly (styles.xml then content.xml) — later styles win. `from_named_section` flags
/// styles as user-facing named even without a display-name (true for `styles.xml`).
/// A self-closing `<style:style/>` (no properties) has no End event, so register it straight
/// from its attributes (parent/outline/heading) rather than through the scratch accumulator.
fn register_self_closing_style(e: &BytesStart, reg: &mut StyleRegistry, from_named_section: bool) {
    if let (Some(nm), Some(fam)) = (attr_val(e, b"style:name"), attr_val(e, b"style:family"))
        && fam == "paragraph"
    {
        let disp = attr_val(e, b"style:display-name");
        let (h, l) = heading_from_name(&nm, disp.as_deref());
        reg.table.para.insert(
            nm,
            ParaStyle {
                parent: attr_val(e, b"style:parent-style-name"),
                outline_level: attr_val(e, b"style:default-outline-level")
                    .and_then(|v| v.parse().ok()),
                heading_name: h,
                name_level: l,
                named: from_named_section || disp.is_some(),
                display_name: disp,
                ..Default::default()
            },
        );
    }
}

/// Per-style-block parse scratch for [`parse_styles`]'s event loop, bundled so it can be split
/// into named handlers below instead of one 200+-line function.
#[derive(Default)]
struct StyleScratch {
    name: Option<String>,
    family: Option<String>,
    parent: Option<String>,
    outline: Option<i64>,
    heading_name: bool,
    name_level: Option<i64>,
    align: Option<String>,
    text: Vec<(String, MarkValue)>,
    display: Option<String>,
    tf: TextFields,
    space_before: Option<f64>,
    space_after: Option<f64>,
    indent: Option<f64>,
    line_height: Option<f64>,
    graphic_wrap: Option<String>,
    /// The style's text properties include one no mark represents.
    text_extra: bool,

    list_name: Option<String>,
    list_numbered: bool,
    list_num_format: Option<&'static str>,
    list_num_start: Option<i64>,
}

impl StyleScratch {
    fn handle_start_or_empty(&mut self, e: &BytesStart) {
        match e.name().as_ref() {
            b"style:style" => {
                self.name = attr_val(e, b"style:name");
                self.family = attr_val(e, b"style:family");
                self.parent = attr_val(e, b"style:parent-style-name");
                self.outline =
                    attr_val(e, b"style:default-outline-level").and_then(|v| v.parse().ok());
                self.display = attr_val(e, b"style:display-name");
                let (h, l) =
                    heading_from_name(self.name.as_deref().unwrap_or(""), self.display.as_deref());
                self.heading_name = h;
                self.name_level = l;
                self.align = None;
                self.text = Vec::new();
                self.tf = TextFields::default();
                self.space_before = None;
                self.space_after = None;
                self.indent = None;
                self.line_height = None;
                self.graphic_wrap = None;
            }
            b"style:graphic-properties" => {
                if let Some(w) = attr_val(e, b"style:wrap") {
                    self.graphic_wrap = Some(map_odf_wrap(&w).to_string());
                }
            }
            b"style:text-properties" => {
                self.text_extra |= has_unmodeled_text_props(e);
                self.text.extend(marks_from_text_props(e));
                let f = text_fields(e);
                if f.size.is_some() {
                    self.tf.size = f.size;
                }
                if f.bold.is_some() {
                    self.tf.bold = f.bold;
                }
                if f.italic.is_some() {
                    self.tf.italic = f.italic;
                }
                if f.color.is_some() {
                    self.tf.color = f.color;
                }
                if f.font.is_some() {
                    self.tf.font = f.font;
                }
            }
            b"style:paragraph-properties" => {
                if let Some(a) = attr_val(e, b"fo:text-align") {
                    self.align = Some(map_align(&a));
                }
                if let Some(v) = attr_val(e, b"fo:margin-top") {
                    self.space_before = parse_margin_px(&v);
                }
                if let Some(v) = attr_val(e, b"fo:margin-bottom") {
                    self.space_after = parse_margin_px(&v);
                }
                if let Some(v) =
                    attr_val(e, b"fo:margin-left").or_else(|| attr_val(e, b"fo:text-indent"))
                {
                    self.indent = parse_margin_px(&v);
                }
                if let Some(v) = attr_val(e, b"fo:line-height") {
                    self.line_height = parse_line_height(&v);
                }
            }
            b"text:list-style" => {
                self.list_name = attr_val(e, b"style:name");
                self.list_numbered = false;
                self.list_num_format = None;
                self.list_num_start = None;
            }
            b"text:list-level-style-number" => {
                self.list_numbered = true;
                // Capture level-1 (0-based level 0) format + start; ODF numbers levels from 1, `text:level="1"` is the model's level 0.
                let is_lvl0 = attr_val(e, b"text:level").is_none_or(|l| l == "1");
                if is_lvl0 {
                    if let Some(f) = attr_val(e, b"style:num-format") {
                        self.list_num_format = Some(map_num_format(&f));
                    }
                    if let Some(s) =
                        attr_val(e, b"text:start-value").and_then(|v| v.parse::<i64>().ok())
                    {
                        self.list_num_start = Some(s);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_end(
        &mut self,
        e: &quick_xml::events::BytesEnd,
        reg: &mut StyleRegistry,
        from_named_section: bool,
    ) {
        match e.name().as_ref() {
            b"style:style" => {
                if let (Some(n), Some(fam)) = (self.name.take(), self.family.take()) {
                    match fam.as_str() {
                        "text" => {
                            if from_named_section || self.parent.is_some() || self.text_extra {
                                reg.text_extra.insert(n.clone());
                            }
                            if !self.text.is_empty() {
                                reg.table.text.insert(n, std::mem::take(&mut self.text));
                            }
                        }
                        "paragraph" => {
                            reg.table.para.insert(
                                n,
                                ParaStyle {
                                    parent: self.parent.take(),
                                    align: self.align.take(),
                                    outline_level: self.outline.take(),
                                    heading_name: self.heading_name,
                                    name_level: self.name_level.take(),
                                    text: std::mem::take(&mut self.text),
                                    named: from_named_section || self.display.is_some(),
                                    display_name: self.display.take(),
                                    size: self.tf.size.take(),
                                    bold: self.tf.bold.take(),
                                    italic: self.tf.italic.take(),
                                    color: self.tf.color.take(),
                                    font: self.tf.font.take(),
                                    space_before: self.space_before.take(),
                                    space_after: self.space_after.take(),
                                    indent: self.indent.take(),
                                    line_height: self.line_height.take(),
                                },
                            );
                        }
                        "graphic" => {
                            if let Some(w) = self.graphic_wrap.take() {
                                reg.graphic_wrap.insert(n, w);
                            }
                        }
                        _ => {}
                    }
                }
                self.parent = None;
                self.outline = None;
                self.heading_name = false;
                self.display = None;
                self.text.clear();
                self.tf = TextFields::default();
                self.space_before = None;
                self.space_after = None;
                self.indent = None;
                self.line_height = None;
                self.graphic_wrap = None;
                self.text_extra = false;
            }
            b"text:list-style" => {
                if let Some(n) = self.list_name.take() {
                    reg.list_ordered.insert(n.clone(), self.list_numbered);
                    reg.list_numbering.insert(
                        n,
                        ListNumbering {
                            num_format: self.list_num_format.take(),
                            num_start: self.list_num_start.take(),
                        },
                    );
                }
                self.list_num_format = None;
                self.list_num_start = None;
            }
            _ => {}
        }
    }
}

/// Text properties the model has marks for (their Asian/complex-script twins follow them), and
/// ones that carry nothing to keep (editing-session ids, the underline's width/colour).
const MODELED_TEXT_PROPS: &[&str] = &[
    "fo:background-color",
    "fo:color",
    "fo:country",
    "fo:font-family",
    "fo:font-size",
    "fo:font-style",
    "fo:font-weight",
    "fo:language",
    "style:font-name",
    "style:text-line-through-style",
    "style:text-position",
    "style:text-underline-style",
    "style:text-underline-width",
    "style:text-underline-color",
    "style:text-line-through-type",
    "officeooo:rsid",
    "officeooo:paragraph-rsid",
];

/// Whether a `style:text-properties` sets anything the model has no mark for.
fn has_unmodeled_text_props(e: &BytesStart) -> bool {
    e.attributes().flatten().any(|a| {
        let key = String::from_utf8_lossy(a.key.as_ref());
        let base = key
            .strip_suffix("-asian")
            .or_else(|| key.strip_suffix("-complex"))
            .unwrap_or(&key);
        let base = base
            .replace("style:language", "fo:language")
            .replace("style:country", "fo:country")
            .replace("style:font-style", "fo:font-style")
            .replace("style:font-weight", "fo:font-weight")
            .replace("style:font-size", "fo:font-size");
        !MODELED_TEXT_PROPS.contains(&base.as_str())
    })
}

pub(super) fn parse_styles(xml: &[u8], reg: &mut StyleRegistry, from_named_section: bool) {
    let Ok(s) = std::str::from_utf8(xml) else {
        return;
    };
    let mut reader = Reader::from_str(s);
    let mut scratch = StyleScratch::default();

    while let Ok(ev) = reader.read_event() {
        match ev {
            // A self-closing <style:style/> (no properties) has no End event, so register it straight from its attributes (parent/outline/heading).
            Event::Empty(e) if e.name().as_ref() == b"style:style" => {
                register_self_closing_style(&e, reg, from_named_section);
            }
            Event::Start(e) | Event::Empty(e) => scratch.handle_start_or_empty(&e),
            Event::End(e) => scratch.handle_end(&e, reg, from_named_section),
            Event::Eof => break,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_and_list_styles_resolve() {
        // Phase `resolves_named_heading_and_text_styles`.
        {
            let xml = br#"<office:document-styles>
            <office:styles>
            <style:style style:name="Heading_20_1" style:display-name="Heading 1" style:family="paragraph" style:default-outline-level="1"><style:paragraph-properties fo:text-align="center"/><style:text-properties fo:font-weight="bold"/></style:style>
            <style:style style:name="MySub" style:family="paragraph" style:parent-style-name="Heading_20_1"/>
            <style:style style:name="Strong" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>
            </office:styles>
            </office:document-styles>"#;
            let mut reg = StyleRegistry::default();
            parse_styles(xml, &mut reg, true);
            assert_eq!(reg.table.heading_level("Heading_20_1", |_| None), Some(1));
            assert_eq!(
                reg.table.heading_level("MySub", |_| None),
                Some(1),
                "inherits heading via parent"
            );
            assert_eq!(
                reg.table.para_align("Heading_20_1").as_deref(),
                Some("center")
            );
            assert!(
                reg.table
                    .text_marks("Strong")
                    .iter()
                    .any(|(n, _)| n == "strong")
            );
            assert!(
                reg.table
                    .para_text_marks("Heading_20_1")
                    .iter()
                    .any(|(n, _)| n == "strong")
            );
        }
        // Phase `detects_ordered_list_style`.
        {
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
}
