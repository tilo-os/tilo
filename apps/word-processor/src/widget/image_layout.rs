//! Pure image-attribute decoding and paint geometry.

use tilo_doc::snapshot::{AttrVal, BlockView};
use vello::kurbo::{Affine, Rect};

use crate::num;

/// An image block's `anchor` attr (`inline` | `paragraph` | `page`).
pub(super) fn anchor(block: &BlockView) -> &str {
    match block.attrs.get("anchor") {
        Some(AttrVal::Str(value)) => value.as_str(),
        _ => "inline",
    }
}

/// An image block's `wrap` attr (`none` | `left` | `right` | `both`).
pub(super) fn wrap(block: &BlockView) -> &str {
    match block.attrs.get("wrap") {
        Some(AttrVal::Str(value)) => value.as_str(),
        _ => "none",
    }
}

/// Source-relative crop insets in `[0, 1)`, decoded from ten-thousandths.
pub(super) fn crop(block: &BlockView) -> (f64, f64, f64, f64) {
    let edge = |key: &str| match block.attrs.get(key) {
        Some(AttrVal::Int(value)) if (0..=9_500).contains(value) => {
            num::f64_of_i64(*value) / 10_000.0
        }
        _ => 0.0,
    };
    let crop = (
        edge("crop_left"),
        edge("crop_top"),
        edge("crop_right"),
        edge("crop_bottom"),
    );
    if crop.0 + crop.2 > 0.95 || crop.1 + crop.3 > 0.95 {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        crop
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct PaintOptions {
    pub(super) flip_horizontal: bool,
    pub(super) flip_vertical: bool,
    pub(super) crop: (f64, f64, f64, f64),
}

pub(super) fn paint_options(block: Option<&BlockView>) -> PaintOptions {
    block.map_or_else(PaintOptions::default, |block| PaintOptions {
        flip_horizontal: matches!(block.attrs.get("flip_h"), Some(AttrVal::Bool(true))),
        flip_vertical: matches!(block.attrs.get("flip_v"), Some(AttrVal::Bool(true))),
        crop: crop(block),
    })
}

pub(super) fn paint_transform(base: Affine, rect: Rect, options: PaintOptions) -> Affine {
    if !options.flip_horizontal && !options.flip_vertical {
        return base;
    }
    let cx = f64::midpoint(rect.x0, rect.x1);
    let cy = f64::midpoint(rect.y0, rect.y1);
    base * Affine::translate((cx, cy))
        * Affine::scale_non_uniform(
            if options.flip_horizontal { -1.0 } else { 1.0 },
            if options.flip_vertical { -1.0 } else { 1.0 },
        )
        * Affine::translate((-cx, -cy))
}

pub(super) fn cropped_source(rect: Rect, crop: (f64, f64, f64, f64)) -> Rect {
    let (left, top, right, bottom) = crop;
    let visible_w = (1.0 - left - right).max(0.05);
    let visible_h = (1.0 - top - bottom).max(0.05);
    Rect::new(
        rect.x0 - rect.width() * left / visible_w,
        rect.y0 - rect.height() * top / visible_h,
        rect.x0 + rect.width() * (1.0 - left) / visible_w,
        rect.y0 + rect.height() * (1.0 - top) / visible_h,
    )
}

pub(super) fn crop_is_active(crop: (f64, f64, f64, f64)) -> bool {
    crop != (0.0, 0.0, 0.0, 0.0)
}

/// A page-relative free object's persisted position.
pub(super) fn free_position(block: &BlockView) -> Option<(f64, f64, usize)> {
    let int = |key: &str| match block.attrs.get(key) {
        Some(AttrVal::Int(value)) => Some(*value),
        _ => None,
    };
    let (x, y, page) = (int("offset_x")?, int("offset_y")?, int("page")?);
    if !(-100_000..=100_000).contains(&x)
        || !(-100_000..=100_000).contains(&y)
        || !(0..=10_000).contains(&page)
    {
        return None;
    }
    Some((
        num::f64_of_i64(x),
        num::f64_of_i64(y),
        num::usize_of_i64(page),
    ))
}

/// Explicit display size `(width, height)` in pixels.
pub(super) fn explicit_size(block: &BlockView) -> Option<(f64, f64)> {
    let dimension = |key: &str| match block.attrs.get(key) {
        Some(AttrVal::Int(value)) if *value > 0 => Some(num::f64_of_i64(*value)),
        Some(AttrVal::Float(value)) if *value > 0.0 => Some(*value),
        _ => None,
    };
    Some((dimension("width")?, dimension("height")?))
}

/// Whether following text should flow beside this image.
pub(super) fn floats(block: &BlockView) -> bool {
    matches!(anchor(block), "paragraph" | "page") && matches!(wrap(block), "left" | "right")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn image(attrs: impl IntoIterator<Item = (&'static str, AttrVal)>) -> BlockView {
        BlockView {
            id: "image-1".into(),
            kind: "image".into(),
            text: String::new(),
            marks: Vec::new(),
            attrs: attrs
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect::<BTreeMap<_, _>>(),
            children: Vec::new(),
        }
    }

    /// Valid attributes decode into layout values and malformed geometry degrades to safe defaults.
    #[test]
    fn image_layout_attributes_decode_or_degrade_safely() {
        // Phase: valid_attributes_decode_into_layout_values.
        {
            let block = image([
                ("anchor", AttrVal::Str("page".into())),
                ("wrap", AttrVal::Str("left".into())),
                ("crop_left", AttrVal::Int(500)),
                ("crop_top", AttrVal::Int(250)),
                ("offset_x", AttrVal::Int(-24)),
                ("offset_y", AttrVal::Int(36)),
                ("page", AttrVal::Int(2)),
                ("width", AttrVal::Int(320)),
                ("height", AttrVal::Float(180.5)),
            ]);

            assert!(floats(&block));
            assert_eq!(crop(&block), (0.05, 0.025, 0.0, 0.0));
            assert_eq!(free_position(&block), Some((-24.0, 36.0, 2)));
            assert_eq!(explicit_size(&block), Some((320.0, 180.5)));
        }

        // Phase: malformed_geometry_degrades_to_safe_defaults.
        {
            let block = image([
                ("crop_left", AttrVal::Int(9_000)),
                ("crop_right", AttrVal::Int(9_000)),
                ("offset_x", AttrVal::Int(100_001)),
                ("offset_y", AttrVal::Int(0)),
                ("page", AttrVal::Int(0)),
                ("width", AttrVal::Int(-1)),
                ("height", AttrVal::Float(f64::NAN)),
            ]);

            assert_eq!(anchor(&block), "inline");
            assert_eq!(wrap(&block), "none");
            assert_eq!(crop(&block), (0.0, 0.0, 0.0, 0.0));
            assert_eq!(free_position(&block), None);
            assert_eq!(explicit_size(&block), None);
            assert!(!floats(&block));
        }
    }
}
