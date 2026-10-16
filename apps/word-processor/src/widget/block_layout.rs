//! Pure block-attribute decoding used by pagination and toolbar state.

use tilo_doc::snapshot::{AttrVal, BlockView};
use ui::richtext::{FontFamily, TextAlign};

use crate::num;

pub(super) const LIST_INDENT_STEP: f64 = 24.0;
pub(super) const LIST_MARKER_TEXT_GAP: f64 = 8.0;
pub(super) const LINE_SPACINGS: [f64; 4] = [1.0, 1.15, 1.5, 2.0];

pub(super) fn page_break_before(block: &BlockView) -> bool {
    matches!(
        block.attrs.get("page_break_before"),
        Some(AttrVal::Bool(true))
    )
}

pub(super) fn line_spacing(block: &BlockView) -> f32 {
    match block.attrs.get("line_spacing") {
        Some(AttrVal::Float(value)) if *value > 0.0 => num::f32_of_f64(*value),
        Some(AttrVal::Int(value)) if *value > 0 => num::f32_of_i64(*value),
        _ => 1.0,
    }
}

/// The name of a line spacing: "Single" and "Double" by name, anything
/// else as its multiplier.
pub(super) fn line_spacing_label(multiplier: f64) -> String {
    if (multiplier - 1.0).abs() < 0.005 {
        crate::i18n::t("line-spacing-single")
    } else if (multiplier - 2.0).abs() < 0.005 {
        crate::i18n::t("line-spacing-double")
    } else if (multiplier - multiplier.round()).abs() < 0.005 {
        format!("{}", num::i64_of_f64(multiplier.round()))
    } else {
        format!("{multiplier:.2}").trim_end_matches('0').to_string()
    }
}

pub(super) fn indent(block: &BlockView) -> f64 {
    match block.attrs.get("indent") {
        Some(AttrVal::Float(value)) => *value,
        Some(AttrVal::Int(value)) => num::f64_of_i64(*value),
        _ => 0.0,
    }
}

pub(super) fn align(block: &BlockView, default: TextAlign) -> TextAlign {
    match block.attrs.get("align") {
        Some(AttrVal::Str(value)) if value == "center" => TextAlign::Center,
        Some(AttrVal::Str(value)) if value == "right" => TextAlign::Right,
        Some(AttrVal::Str(value)) if value == "left" => TextAlign::Left,
        _ => default,
    }
}

pub(super) fn font(name: &str) -> FontFamily {
    match name {
        "serif" => FontFamily::Serif,
        "mono" => FontFamily::Mono,
        _ => FontFamily::Sans,
    }
}

pub(super) fn style_align(value: &str) -> TextAlign {
    match value {
        "center" => TextAlign::Center,
        "right" => TextAlign::Right,
        _ => TextAlign::Left,
    }
}

pub(super) fn style_key(block: &BlockView) -> String {
    let level = block.int_attr("level");
    let explicit = block.str_attr("style");
    crate::styles::style_key_for(&block.kind, level, explicit)
}

pub(super) fn list_level(block: &BlockView) -> usize {
    match block.attrs.get("level") {
        Some(AttrVal::Int(value)) if *value > 0 => num::usize_of_i64(*value),
        _ => 0,
    }
}

pub(super) fn list_marker_x(text_x: f64, measured_width: f64) -> f64 {
    text_x - LIST_MARKER_TEXT_GAP - measured_width
}

pub(super) fn bullet_glyph(level: usize) -> &'static str {
    match level % 3 {
        0 => "•",
        1 => "◦",
        _ => "▪",
    }
}

pub(super) fn cell_table(block: &BlockView) -> Option<String> {
    block.str_attr("table").map(str::to_owned)
}

pub(super) fn cell_rc(block: &BlockView) -> (usize, usize) {
    let coordinate = |key: &str| match block.attrs.get(key) {
        Some(AttrVal::Int(value)) if *value >= 0 => num::usize_of_i64(*value),
        _ => 0,
    };
    (coordinate("row"), coordinate("col"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn block(attrs: impl IntoIterator<Item = (&'static str, AttrVal)>) -> BlockView {
        BlockView {
            id: "block-1".into(),
            kind: "paragraph".into(),
            text: String::new(),
            marks: Vec::new(),
            attrs: attrs
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect::<BTreeMap<_, _>>(),
            children: Vec::new(),
        }
    }

    /// Imported values resolve into UI layout types, and malformed numeric attributes use safe defaults.
    #[test]
    fn block_layout_attributes_resolve_or_fall_back_safely() {
        // Phase: imported_values_resolve_into_ui_layout_types.
        {
            let block = block([
                ("page_break_before", AttrVal::Bool(true)),
                ("line_spacing", AttrVal::Float(1.5)),
                ("indent", AttrVal::Int(24)),
                ("align", AttrVal::Str("right".into())),
                ("table", AttrVal::Str("table-1".into())),
                ("row", AttrVal::Int(2)),
                ("col", AttrVal::Int(3)),
            ]);

            assert!(page_break_before(&block));
            assert_eq!(line_spacing(&block), 1.5);
            assert_eq!(indent(&block), 24.0);
            assert_eq!(align(&block, TextAlign::Left), TextAlign::Right);
            assert_eq!(cell_table(&block).as_deref(), Some("table-1"));
            assert_eq!(cell_rc(&block), (2, 3));
        }

        // Phase: malformed_numeric_attributes_use_safe_layout_defaults.
        {
            let block = block([
                ("line_spacing", AttrVal::Float(f64::NAN)),
                ("level", AttrVal::Int(-4)),
                ("row", AttrVal::Int(-1)),
                ("col", AttrVal::Str("three".into())),
            ]);

            assert_eq!(line_spacing(&block), 1.0);
            assert_eq!(list_level(&block), 0);
            assert_eq!(cell_rc(&block), (0, 0));
        }
    }
}
