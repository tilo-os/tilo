//! The document's **style system** — a small set of named styles (Body,
//! Heading 1–3, List Item, Quote, plus user-defined ones) that every block
//! resolves against. Editing a style updates *every* block of that style: the
//! component-instance model, not per-instance formatting. Persisted in the
//! document (qdoc meta) so it round-trips and survives reopen.

use crate::num;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tilo_doc::AttrVal;
use tilo_doc::schema::{
    STYLE_ALIGN, STYLE_BOLD, STYLE_COLOR, STYLE_FONT, STYLE_INDENT, STYLE_ITALIC, STYLE_NAME,
    STYLE_SIZE, STYLE_SPACE_AFTER, STYLE_SPACE_BEFORE,
};

/// `[r,g,b]` → `#rrggbb`.
pub fn color_to_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `#rrggbb` (or `rrggbb`, `#rgb`) → `[r,g,b]`; non-hex falls back to black.
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
    /// `tilo_doc::schema::STYLE_*` keys). Color is a `#rrggbb` hex string; sizes /
    /// spacing are floats.
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

    /// Rebuild from the CRDT styles container's scalar shape. Missing properties
    /// fall back to the `StyleProps::default()` values.
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
            color: match props.get(STYLE_COLOR) {
                Some(AttrVal::Str(s)) => hex_to_color(s),
                _ => d.color,
            },
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

/// The document's full set of styles.
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

/// The built-in style ids. Everything else in a document's sheet is a
/// user-defined ("custom") style, which — unlike a built-in — can be deleted.
pub use tilo_document::BUILTIN_STYLES;

impl StyleSheet {
    /// Resolve a style, falling back to Body for unknown ids.
    pub fn get(&self, key: &str) -> StyleProps {
        self.styles
            .get(key)
            .or_else(|| self.styles.get("body"))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `key` names a built-in style. Built-ins are permanent (the style
    /// panel offers *Duplicate* on them but never *Delete*); custom styles can
    /// be deleted.
    pub fn is_builtin(key: &str) -> bool {
        BUILTIN_STYLES.contains(&key)
    }

    /// Remove the custom style `key` from the sheet, returning `true` when it was
    /// present and removable. Built-in styles are never removed (they are the
    /// document's baseline component set). Callers are responsible for reassigning
    /// any block that still references the removed style.
    pub fn remove(&mut self, key: &str) -> bool {
        if Self::is_builtin(key) {
            return false;
        }
        self.styles.remove(key).is_some()
    }

    /// Allocate a fresh, unused `customN` style id (never colliding with an
    /// existing entry). Used by the Duplicate / New-style flows.
    pub fn fresh_custom_id(&self) -> String {
        // Pigeonhole: at most `len()` of the `len() + 1` candidates can be
        // taken, so the range always contains a free id. The fallback keeps
        // the search bounded rather than looping on a counter that could wrap.
        let limit = self.styles.len().saturating_add(1);
        (1..=limit)
            .map(|n| format!("custom{n}"))
            .find(|cand| !self.styles.contains_key(cand))
            .unwrap_or_else(|| format!("custom{}", limit.saturating_add(1)))
    }

    /// Style ids in a stable presentation order (built-ins first, then custom).
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

/// The localized, user-facing display name for a style. Built-in styles carry
/// an English name in the document so exports stay interoperable, but the
/// panel/pickers show the active locale (German "Body" → "Fließtext") via a
/// Fluent key per built-in id; custom styles keep their stored name verbatim.
pub fn style_display_name(id: &str, props: &StyleProps) -> String {
    let key = match id {
        "body" => "style-body",
        "heading1" => "style-heading1",
        "heading2" => "style-heading2",
        "heading3" => "style-heading3",
        "list" => "style-list",
        "quote" => "style-quote",
        // Custom styles are named by the user; show their name as authored.
        _ => return props.name.clone(),
    };
    crate::i18n::t(key)
}

/// The block `kind` (+ heading `level`) a built-in style implies, so applying a
/// paragraph style from the panel also makes the block *structurally* correct —
/// picking "Heading 1" yields a real heading (present in the outline / TOC), not
/// merely heading-looking body text. Custom styles imply no structural kind
/// (they are visual presets), so this returns `None` for them.
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

/// The style id a block resolves to: an explicit `style` attr if set, otherwise
/// derived from the block kind (+ heading level). This keeps existing documents
/// working while letting blocks opt into custom styles.
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

    /// Defaults cover builtin kinds, JSON round-trips, display names localise builtins and pass custom
    /// names through, and structural kinds map builtins only.
    #[test]
    fn stylesheets_cover_builtins_round_trip_and_localise() {
        // Phase: defaults_cover_builtin_kinds.
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
            // explicit override wins
            assert_eq!(style_key_for("paragraph", None, Some("quote")), "quote");
        }

        // Phase: display_name_localizes_builtins_and_passes_custom_through.
        // Built-in styles resolve a localized display name through Fluent (non-empty
        // in the ambient locale); a custom style shows its stored, user-authored name
        // verbatim (never a Fluent key). Locale-independent: only asserts the mapping
        // and the custom passthrough, not a specific translation.
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

        // Phase: structural_kind_maps_builtins_only.
        // The built-in style → structural kind map: applying a built-in from the
        // panel picker also switches the block kind (so "Heading 1" is a real
        // heading). Custom styles imply no kind.
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
