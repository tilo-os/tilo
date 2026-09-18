//! Stable cache identity for shaped rich-text layouts.

use std::hash::{Hash, Hasher};

use ui::richtext::{FontFamily, RichSpan, TextAlign};

#[derive(Clone, Copy)]
pub(super) struct ShapeKeyInput<'a> {
    pub(super) spans: &'a [RichSpan],
    pub(super) underlines: &'a [(usize, usize)],
    pub(super) strikes: &'a [(usize, usize)],
    pub(super) base_bold: bool,
    pub(super) size: f32,
    pub(super) wrap_w: f32,
    pub(super) align: TextAlign,
    pub(super) family: FontFamily,
    pub(super) line_height_mult: f32,
}

pub(super) fn shape_key(input: &ShapeKeyInput<'_>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.spans.len().hash(&mut hasher);
    for span in input.spans {
        span.text.hash(&mut hasher);
        span.bold.hash(&mut hasher);
        span.italic.hash(&mut hasher);
        span.size.map(f32::to_bits).hash(&mut hasher);
        span.family.map(|family| family as u8).hash(&mut hasher);
        span.color.hash(&mut hasher);
        span.background.hash(&mut hasher);
        (span.baseline as u8).hash(&mut hasher);
    }
    input.underlines.hash(&mut hasher);
    input.strikes.hash(&mut hasher);
    input.base_bold.hash(&mut hasher);
    input.size.to_bits().hash(&mut hasher);
    input.wrap_w.to_bits().hash(&mut hasher);
    (input.align as u8).hash(&mut hasher);
    (input.family as u8).hash(&mut hasher);
    input.line_height_mult.to_bits().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use ui::richtext::BaselineShift;

    use super::*;

    fn key(span: &RichSpan, wrap_w: f32) -> u64 {
        shape_key(&ShapeKeyInput {
            spans: std::slice::from_ref(span),
            underlines: &[],
            strikes: &[],
            base_bold: false,
            size: 14.0,
            wrap_w,
            align: TextAlign::Left,
            family: FontFamily::Sans,
            line_height_mult: 1.0,
        })
    }

    #[test]
    fn visual_run_and_width_changes_invalidate_the_shape_key() {
        let plain = RichSpan::new("same text");
        let mut highlighted = plain.clone();
        highlighted.background = Some([255, 240, 0]);
        let mut superscript = plain.clone();
        superscript.baseline = BaselineShift::Super;

        let base = key(&plain, 400.0);
        assert_ne!(base, key(&highlighted, 400.0));
        assert_ne!(base, key(&superscript, 400.0));
        assert_ne!(base, key(&plain, 320.0));
        assert_eq!(base, key(&plain, 400.0));
    }
}
