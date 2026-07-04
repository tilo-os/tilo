//! The document's **style system** — a small set of named styles (Body,
//! Heading 1–3, List Item, Quote, plus user-defined ones) that every block
//! resolves against. Editing a style updates *every* block of that style: the
//! component-instance model, not per-instance formatting. Persisted in the
//! document (qdoc meta) so it round-trips and survives reopen.

use qdoc::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME, STYLE_SIZE, STYLE_SPACE_AFTER,
    STYLE_SPACE_BEFORE,
};
use qdoc::AttrVal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `[r,g,b]` → `#rrggbb`.
pub fn color_to_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `#rrggbb` (or `rrggbb`) → `[r,g,b]`; non-hex falls back to black.
pub fn hex_to_color(s: &str) -> [u8; 3] {
    let h = s.trim().trim_start_matches('#');
    if h.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&h[0..2], 16),
            u8::from_str_radix(&h[2..4], 16),
            u8::from_str_radix(&h[4..6], 16),
        ) {
            return [r, g, b];
        }
    }
    [0, 0, 0]
}

fn attr_f32(props: &BTreeMap<String, AttrVal>, key: &str, default: f32) -> f32 {
    match props.get(key) {
        Some(AttrVal::Float(f)) => *f as f32,
        Some(AttrVal::Int(n)) => *n as f32,
        _ => default,
    }
}

fn attr_str(props: &BTreeMap<String, AttrVal>, key: &str, default: &str) -> String {
    match props.get(key) {
        Some(AttrVal::Str(s)) => s.clone(),
        _ => default.to_string(),
    }
}

fn attr_bool(props: &BTreeMap<String, AttrVal>, key: &str) -> bool {
    matches!(props.get(key), Some(AttrVal::Bool(true)))
}

/// Every visual property of a style.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct StyleProps {
    pub name: String,
    /// "sans" | "serif" | "mono".
    pub font: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: [u8; 3],
    /// Default alignment ("left" | "center" | "right"); an instance `align` attr overrides.
    pub align: String,
    pub space_before: f32,
    pub space_after: f32,
    /// Left text indent, px.
    pub indent: f32,
}

impl StyleProps {
    /// Serialize into the CRDT styles container's scalar shape (the
    /// `qdoc::schema::STYLE_*` keys). Color is a `#rrggbb` hex string; sizes /
    /// spacing are floats.
    pub fn to_attrs(&self) -> BTreeMap<String, AttrVal> {
        let mut m = BTreeMap::new();
        m.insert(STYLE_NAME.into(), AttrVal::Str(self.name.clone()));
        m.insert(STYLE_FONT.into(), AttrVal::Str(self.font.clone()));
        m.insert(STYLE_SIZE.into(), AttrVal::Float(self.size as f64));
        m.insert(STYLE_BOLD.into(), AttrVal::Bool(self.bold));
        m.insert(STYLE_ITALIC.into(), AttrVal::Bool(self.italic));
        m.insert(STYLE_COLOR.into(), AttrVal::Str(color_to_hex(self.color)));
        m.insert(STYLE_ALIGN.into(), AttrVal::Str(self.align.clone()));
        m.insert(STYLE_SPACE_BEFORE.into(), AttrVal::Float(self.space_before as f64));
        m.insert(STYLE_SPACE_AFTER.into(), AttrVal::Float(self.space_after as f64));
        m.insert(STYLE_INDENT.into(), AttrVal::Float(self.indent as f64));
        m
    }

    /// Rebuild from the CRDT styles container's scalar shape. Missing properties
    /// fall back to the `StyleProps::default()` values.
    pub fn from_attrs(props: &BTreeMap<String, AttrVal>) -> Self {
        let d = StyleProps::default();
        StyleProps {
            name: attr_str(props, STYLE_NAME, &d.name),
            font: attr_str(props, STYLE_FONT, if d.font.is_empty() { "sans" } else { &d.font }),
            size: attr_f32(props, STYLE_SIZE, d.size),
            bold: attr_bool(props, STYLE_BOLD),
            italic: attr_bool(props, STYLE_ITALIC),
            color: match props.get(STYLE_COLOR) {
                Some(AttrVal::Str(s)) => hex_to_color(s),
                _ => d.color,
            },
            align: attr_str(props, STYLE_ALIGN, if d.align.is_empty() { "left" } else { &d.align }),
            space_before: attr_f32(props, STYLE_SPACE_BEFORE, d.space_before),
            space_after: attr_f32(props, STYLE_SPACE_AFTER, d.space_after),
            indent: attr_f32(props, STYLE_INDENT, d.indent),
        }
    }
}

/// The document's full set of styles.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StyleSheet {
    pub styles: BTreeMap<String, StyleProps>,
}

const INK: [u8; 3] = [0x1c, 0x1c, 0x20];

fn base(name: &str, font: &str, size: f32, bold: bool, color: [u8; 3], after: f32, indent: f32) -> StyleProps {
    StyleProps {
        name: name.into(),
        font: font.into(),
        size,
        bold,
        italic: false,
        color,
        align: "left".into(),
        space_before: 0.0,
        space_after: after,
        indent,
    }
}

impl Default for StyleSheet {
    fn default() -> Self {
        let mut s = BTreeMap::new();
        s.insert("body".into(), base("Body", "sans", 16.5, false, INK, 8.0, 0.0));
        s.insert("heading1".into(), base("Heading 1", "sans", 30.0, true, INK, 10.0, 0.0));
        s.insert("heading2".into(), base("Heading 2", "sans", 25.0, true, INK, 8.0, 0.0));
        s.insert("heading3".into(), base("Heading 3", "sans", 21.0, true, INK, 6.0, 0.0));
        s.insert("list".into(), base("List Item", "sans", 16.5, false, INK, 4.0, 28.0));
        let mut quote = base("Quote", "serif", 17.0, false, [0x55, 0x57, 0x60], 10.0, 22.0);
        quote.italic = true;
        s.insert("quote".into(), quote);
        StyleSheet { styles: s }
    }
}

impl StyleSheet {
    /// Resolve a style, falling back to Body for unknown ids.
    pub fn get(&self, key: &str) -> StyleProps {
        self.styles
            .get(key)
            .or_else(|| self.styles.get("body"))
            .cloned()
            .unwrap_or_default()
    }

    /// Style ids in a stable presentation order (built-ins first, then custom).
    pub fn order(&self) -> Vec<String> {
        const BUILTIN: [&str; 6] = ["body", "heading1", "heading2", "heading3", "list", "quote"];
        let mut out: Vec<String> = BUILTIN.iter().filter(|k| self.styles.contains_key(**k)).map(|k| k.to_string()).collect();
        for k in self.styles.keys() {
            if !BUILTIN.contains(&k.as_str()) {
                out.push(k.clone());
            }
        }
        out
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

/// The style id a block resolves to: an explicit `style` attr if set, otherwise
/// derived from the block kind (+ heading level). This keeps existing documents
/// working while letting blocks opt into custom styles.
pub fn style_key_for(kind: &str, level: Option<i64>, explicit: Option<&str>) -> String {
    if let Some(s) = explicit {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    match kind {
        "heading" => format!("heading{}", level.unwrap_or(1).clamp(1, 3)),
        "list_item" => "list".to_string(),
        _ => "body".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_builtin_kinds() {
        let sheet = StyleSheet::default();
        assert_eq!(sheet.get(&style_key_for("heading", Some(1), None)).name, "Heading 1");
        assert_eq!(sheet.get(&style_key_for("heading", Some(2), None)).name, "Heading 2");
        assert_eq!(sheet.get(&style_key_for("list_item", None, None)).name, "List Item");
        assert_eq!(sheet.get(&style_key_for("paragraph", None, None)).name, "Body");
        // explicit override wins
        assert_eq!(style_key_for("paragraph", None, Some("quote")), "quote");
    }

    #[test]
    fn json_round_trips() {
        let sheet = StyleSheet::default();
        let back = StyleSheet::from_json(&sheet.to_json()).unwrap();
        assert_eq!(back.get("body"), sheet.get("body"));
        assert_eq!(back.get("quote").italic, sheet.get("quote").italic);
    }
}
