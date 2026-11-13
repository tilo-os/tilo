//! The document's named styles, which every block resolves against: editing one changes
//! every block of it. Stored in the document's meta, so it survives a save and reopen.

use crate::num;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_doc::AttrVal;
use tilo_doc::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME,
    STYLE_SIZE, STYLE_SPACE_AFTER, STYLE_SPACE_BEFORE,
};

pub fn color_to_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

pub fn hex_to_color(s: &str) -> [u8; 3] {
    ui::color::parse_hex(s).map_or([0, 0, 0], ui::color::to_rgb8)
}

fn attr_f32(props: &BTreeMap<String, AttrVal>, key: &str, default: f32) -> f32 {
    match props.get(key) {
        Some(AttrVal::Float(f)) => num::f32_of_f64(*f),
        Some(AttrVal::Int(n)) => num::f32_of_i64(*n),
        _ => default,
    }
}

fn attr_str(props: &BTreeMap<String, AttrVal>, key: &str, default: &str) -> String {
    props
        .get(key)
        .and_then(AttrVal::as_str)
        .unwrap_or(default)
        .to_string()
}

fn attr_bool(props: &BTreeMap<String, AttrVal>, key: &str) -> bool {
    props.get(key).and_then(AttrVal::as_bool) == Some(true)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct StyleProps {
    pub name: String,
    pub font: String,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: [u8; 3],
    pub align: String,
    pub space_before: f32,
    pub space_after: f32,
    /// Left text indent, px.
    pub indent: f32,
}

impl StyleProps {
    pub fn to_attrs(&self) -> BTreeMap<String, AttrVal> {
        let mut m = BTreeMap::new();
        m.insert(STYLE_NAME.into(), AttrVal::Str(self.name.clone()));
        m.insert(STYLE_FONT.into(), AttrVal::Str(self.font.clone()));
        m.insert(STYLE_SIZE.into(), AttrVal::Float(f64::from(self.size)));
        m.insert(STYLE_BOLD.into(), AttrVal::Bool(self.bold));
        m.insert(STYLE_ITALIC.into(), AttrVal::Bool(self.italic));
        m.insert(STYLE_COLOR.into(), AttrVal::Str(color_to_hex(self.color)));
        m.insert(STYLE_ALIGN.into(), AttrVal::Str(self.align.clone()));
        m.insert(
            STYLE_SPACE_BEFORE.into(),
            AttrVal::Float(f64::from(self.space_before)),
        );
        m.insert(
            STYLE_SPACE_AFTER.into(),
            AttrVal::Float(f64::from(self.space_after)),
        );
        m.insert(STYLE_INDENT.into(), AttrVal::Float(f64::from(self.indent)));
        m
    }

    pub fn from_attrs(props: &BTreeMap<String, AttrVal>) -> Self {
        let d = StyleProps::default();
        StyleProps {
            name: attr_str(props, STYLE_NAME, &d.name),
            font: attr_str(
                props,
                STYLE_FONT,
                if d.font.is_empty() { "sans" } else { &d.font },
            ),
            size: attr_f32(props, STYLE_SIZE, d.size),
            bold: attr_bool(props, STYLE_BOLD),
            italic: attr_bool(props, STYLE_ITALIC),
            color: props
                .get(STYLE_COLOR)
                .and_then(AttrVal::as_str)
                .map_or(d.color, hex_to_color),
            align: attr_str(
                props,
                STYLE_ALIGN,
                if d.align.is_empty() { "left" } else { &d.align },
            ),
            space_before: attr_f32(props, STYLE_SPACE_BEFORE, d.space_before),
            space_after: attr_f32(props, STYLE_SPACE_AFTER, d.space_after),
            indent: attr_f32(props, STYLE_INDENT, d.indent),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StyleSheet {
    pub styles: BTreeMap<String, StyleProps>,
}

const INK: [u8; 3] = [0x1c, 0x1c, 0x20];

fn base(
    name: &str,
    font: &str,
    size: f32,
    bold: bool,
    color: [u8; 3],
    after: f32,
    indent: f32,
) -> StyleProps {
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
        s.insert(
            "body".into(),
            base("Body", "sans", 16.5, false, INK, 8.0, 0.0),
        );
        s.insert(
            "heading1".into(),
            base("Heading 1", "sans", 30.0, true, INK, 10.0, 0.0),
        );
        s.insert(
            "heading2".into(),
            base("Heading 2", "sans", 25.0, true, INK, 8.0, 0.0),
        );
        s.insert(
            "heading3".into(),
            base("Heading 3", "sans", 21.0, true, INK, 6.0, 0.0),
        );
        s.insert(
            "list".into(),
            base("List Item", "sans", 16.5, false, INK, 4.0, 28.0),
        );
        let mut quote = base(
            "Quote",
            "serif",
            17.0,
            false,
            [0x55, 0x57, 0x60],
            10.0,
            22.0,
        );
        quote.italic = true;
        s.insert("quote".into(), quote);
        StyleSheet { styles: s }
    }
}

pub use tilo_document::BUILTIN_STYLES;

impl StyleSheet {
    pub fn get(&self, key: &str) -> StyleProps {
        self.styles
            .get(key)
            .or_else(|| self.styles.get("body"))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `key` names a built-in style, which never leaves the sheet.
    pub fn is_builtin(key: &str) -> bool {
        BUILTIN_STYLES.contains(&key)
    }

    /// Remove the custom style `key`, answering whether it was there to remove; a
    /// built-in never goes. The caller reassigns the blocks that referenced it.
    pub fn remove(&mut self, key: &str) -> bool {
        if Self::is_builtin(key) {
            return false;
        }
        self.styles.remove(key).is_some()
    }

    pub fn fresh_custom_id(&self) -> String {
        // At most `len()` of the `len() + 1` candidates are taken, so one is free.
        let limit = self.styles.len().saturating_add(1);
        (1..=limit)
            .map(|n| format!("custom{n}"))
            .find(|cand| !self.styles.contains_key(cand))
            .unwrap_or_else(|| format!("custom{}", limit.saturating_add(1)))
    }

    pub fn order(&self) -> Vec<String> {
        let mut out: Vec<String> = BUILTIN_STYLES
            .iter()
            .filter(|k| self.styles.contains_key(**k))
            .map(std::string::ToString::to_string)
            .collect();
        for k in self.styles.keys() {
            if !BUILTIN_STYLES.contains(&k.as_str()) {
                out.push(k.clone());
            }
        }
        out
    }
}

pub fn style_display_name(id: &str, props: &StyleProps) -> String {
    let key = match id {
        "body" => "style-body",
        "heading1" => "style-heading1",
        "heading2" => "style-heading2",
        "heading3" => "style-heading3",
        "list" => "style-list",
        "quote" => "style-quote",
        _ => return props.name.clone(),
    };
    crate::i18n::t(key)
}

pub fn structural_kind_for(style_id: &str) -> Option<(&'static str, Option<i64>, Option<bool>)> {
    match style_id {
        "body" | "quote" => Some(("paragraph", None, None)),
        "heading1" => Some(("heading", Some(1), None)),
        "heading2" => Some(("heading", Some(2), None)),
        "heading3" => Some(("heading", Some(3), None)),
        "list" => Some(("list_item", None, Some(false))),
        _ => None,
    }
}

pub fn style_key_for(kind: &str, level: Option<i64>, explicit: Option<&str>) -> String {
    if let Some(s) = explicit
        && !s.is_empty()
    {
        return s.to_string();
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
    fn stylesheets_cover_builtins_round_trip_and_localise() {
        {
            let sheet = StyleSheet::default();
            assert_eq!(
                sheet.get(&style_key_for("heading", Some(1), None)).name,
                "Heading 1"
            );
            assert_eq!(
                sheet.get(&style_key_for("heading", Some(2), None)).name,
                "Heading 2"
            );
            assert_eq!(
                sheet.get(&style_key_for("list_item", None, None)).name,
                "List Item"
            );
            assert_eq!(
                sheet.get(&style_key_for("paragraph", None, None)).name,
                "Body"
            );
            assert_eq!(style_key_for("paragraph", None, Some("quote")), "quote");
        }

        {
            let sheet = StyleSheet::default();
            for id in BUILTIN_STYLES {
                let shown = style_display_name(id, &sheet.get(id));
                assert!(
                    !shown.is_empty(),
                    "{id} resolves to a non-empty display name"
                );
            }
            let custom = StyleProps {
                name: "My Callout".into(),
                ..StyleProps::default()
            };
            assert_eq!(
                style_display_name("custom1", &custom),
                "My Callout",
                "custom styles show their authored name, not a Fluent key"
            );
        }

        {
            assert_eq!(structural_kind_for("body"), Some(("paragraph", None, None)));
            assert_eq!(
                structural_kind_for("quote"),
                Some(("paragraph", None, None))
            );
            assert_eq!(
                structural_kind_for("heading1"),
                Some(("heading", Some(1), None))
            );
            assert_eq!(
                structural_kind_for("heading2"),
                Some(("heading", Some(2), None))
            );
            assert_eq!(
                structural_kind_for("heading3"),
                Some(("heading", Some(3), None))
            );
            assert_eq!(
                structural_kind_for("list"),
                Some(("list_item", None, Some(false)))
            );
            assert_eq!(
                structural_kind_for("custom1"),
                None,
                "custom styles imply no structural kind"
            );
        }
    }
}
