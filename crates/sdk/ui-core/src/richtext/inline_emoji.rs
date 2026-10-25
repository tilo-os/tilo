//! A workspace's own emoji inside text: `:name:` drawn as its picture.
//!
//! A run that writes the `:name:` of an emoji the window's workspace has
//! ([`crate::workspace_emoji`]) keeps its bytes, so selection, the caret,
//! copying, search and hit-testing all read the real `:name:`. It is shaped
//! to take one emoji square instead of its letters' width: its glyphs are set
//! at a size too small to see or to measure and spaced out to the square's
//! advance, so the row keeps its height and wraps as if the picture were one
//! glyph. The text renderer leaves those glyphs out and paints the picture
//! into the run's box, centred on the row as the letters around it are.
//!
//! Text whose workspace has no such emoji, or whose picture is not decoded
//! yet, keeps `:name:` as text.

use std::ops::Range;

use cosmic_text::{Attrs, FeatureTag, FontFeatures, Metrics};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::{FontFamily, RichSpan, RichTextLayout};
use crate::workspace_emoji::WorkspaceEmoji;

/// How far an inline emoji advances, in ems of the text it stands in: its
/// picture and a hair of room either side.
pub const ADVANCE_EM: f32 = 1.5;
/// The side of an inline emoji's picture, in ems of its text, at most the
/// height of its row.
pub const SIDE_EM: f32 = 1.375;
/// The size an inline emoji's own glyphs are shaped at: so small that their
/// advances and ink vanish beside the square they are spaced out to.
const SHAPE_SIZE: f32 = 1.0 / 128.0;

/// One inline emoji of a layout: the bytes of its `:name:`, and the side its
/// picture is drawn at.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InlineEmoji {
    pub(crate) range: Range<usize>,
    pub(crate) name: String,
    pub(crate) side: f32,
}

/// Where `text` writes the `:name:` of an emoji `emoji` has a picture for,
/// colons included, in order. A colon that closes one name never opens the
/// next.
#[must_use]
pub fn emoji_ranges(text: &str, emoji: &WorkspaceEmoji) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    if emoji.is_empty() {
        return found;
    }
    let bytes = text.as_bytes();
    let mut at = 0;
    while let Some(open) = bytes
        .get(at..)
        .and_then(|rest| rest.iter().position(|&b| b == b':'))
        .map(|offset| at + offset)
    {
        let name_start = open + 1;
        let name_len = bytes
            .get(name_start..)
            .unwrap_or_default()
            .iter()
            .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_')
            .count();
        let close = name_start + name_len;
        let name = text.get(name_start..close).unwrap_or_default();
        if bytes.get(close) == Some(&b':')
            && tilo_resource::valid_custom_emoji_name(name)
            && emoji.image(name).is_some()
        {
            found.push(open..close + 1);
            at = close + 1;
        } else {
            // The colon that ended this attempt may open the next name.
            at = if bytes.get(close) == Some(&b':') && name_len > 0 {
                close
            } else {
                name_start
            };
        }
    }
    found
}

/// `spans` with every `:name:` of an emoji `emoji` has split out as a run of
/// its own marked [`RichSpan::workspace_emoji`], in the style of the run it
/// was written in. Code (monospaced runs) stays as written.
#[must_use]
pub fn split_spans(spans: Vec<RichSpan>, emoji: &WorkspaceEmoji) -> Vec<RichSpan> {
    if emoji.is_empty() {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len());
    for span in spans {
        let ranges = if span.workspace_emoji || span.family == Some(FontFamily::Mono) {
            Vec::new()
        } else {
            emoji_ranges(&span.text, emoji)
        };
        if ranges.is_empty() {
            out.push(span);
            continue;
        }
        let piece = |range: Range<usize>, picture: bool| {
            let mut piece = span.clone();
            span.text
                .get(range)
                .unwrap_or_default()
                .clone_into(&mut piece.text);
            piece.workspace_emoji = picture;
            piece
        };
        let mut at = 0;
        for range in ranges {
            if at < range.start {
                out.push(piece(at..range.start, false));
            }
            at = range.end;
            out.push(piece(range, true));
        }
        if at < span.text.len() {
            out.push(piece(at..span.text.len(), false));
        }
    }
    out
}

/// [`split_spans`] with the emoji of the window whose code is running.
#[must_use]
pub fn split_spans_in_window(spans: Vec<RichSpan>) -> Vec<RichSpan> {
    split_spans(spans, &crate::workspace_emoji::emoji())
}

/// Whether `text` writes any `:name:` the window's workspace has a picture for.
#[must_use]
pub fn writes_window_emoji(text: &str) -> bool {
    text.contains(':') && !emoji_ranges(text, &crate::workspace_emoji::emoji()).is_empty()
}

/// The attributes an inline emoji's run of `glyphs` glyphs is shaped with,
/// from those of its text at `size` with rows `line_height` tall: the
/// glyphs shrunk to nothing and spaced to one emoji square, the row as tall
/// as the text's. Ligatures stay off so every byte of the ASCII name is one
/// glyph and takes its share of the square.
pub(crate) fn shaped_attrs(
    attrs: Attrs<'_>,
    size: f32,
    line_height: f32,
    glyphs: usize,
) -> Attrs<'_> {
    let glyphs = u16::try_from(glyphs.max(1)).unwrap_or(u16::MAX);
    let mut features = FontFeatures::new();
    for tag in [b"liga", b"clig", b"dlig", b"calt", b"kern"] {
        features.set(FeatureTag::new(tag), 0);
    }
    attrs
        .metrics(Metrics::new(SHAPE_SIZE, line_height))
        .letter_spacing(ADVANCE_EM * size / f32::from(glyphs) / SHAPE_SIZE)
        .font_features(features)
}

impl RichTextLayout {
    /// Whether byte `byte` of the text is drawn as an inline emoji's
    /// picture rather than as a glyph.
    pub(crate) fn in_inline_emoji(&self, byte: usize) -> bool {
        self.inline_emoji
            .iter()
            .any(|emoji| emoji.range.contains(&byte))
    }

    /// Every inline emoji of the layout: its name and the square its picture
    /// fills, in layout coordinates, centred in the box its `:name:` takes
    /// on its row.
    #[must_use]
    pub fn inline_emoji_squares(&self) -> Vec<(&str, Rect)> {
        self.inline_emoji_boxes()
            .into_iter()
            .map(|(_, name, square)| (name, square))
            .collect()
    }

    /// [`Self::inline_emoji_squares`] with the bytes of each `:name:`.
    fn inline_emoji_boxes(&self) -> Vec<(Range<usize>, &str, Rect)> {
        self.inline_emoji
            .iter()
            .filter_map(|emoji| {
                let (x0, y0, x1, y1) = self
                    .selection_rects(emoji.range.start, emoji.range.end)
                    .into_iter()
                    .next()?;
                let side = f64::from(emoji.side.min(y1 - y0).max(0.0));
                let center = (f64::from(x0 + x1) / 2.0, f64::from(y0 + y1) / 2.0);
                let square = Rect::from_center_size(center, (side, side));
                Some((emoji.range.clone(), emoji.name.as_str(), square))
            })
            .collect()
    }

    /// Tint, in `tint`, the pictures of the inline emoji `selected` covers:
    /// a selection is painted under the text, where a picture hides it.
    pub(crate) fn tint_selected_emoji(
        &self,
        scene: &mut Scene,
        transform: Affine,
        selected: Range<usize>,
        tint: vello::peniko::Color,
    ) {
        if self.inline_emoji.is_empty() || selected.is_empty() {
            return;
        }
        for (range, _, square) in self.inline_emoji_boxes() {
            if range.start < selected.end && selected.start < range.end {
                let shape = vello::kurbo::RoundedRect::from_rect(square, square.width() / 5.0);
                scene.fill(vello::peniko::Fill::NonZero, transform, tint, None, &shape);
            }
        }
    }

    /// Paint the pictures of the layout's inline emoji, drawn at `transform`
    /// as its glyphs are, from the window's workspace emoji.
    pub(crate) fn paint_inline_emoji(&self, scene: &mut Scene, transform: Affine) {
        if self.inline_emoji.is_empty() {
            return;
        }
        let emoji = crate::workspace_emoji::emoji();
        for (name, square) in self.inline_emoji_squares() {
            if let Some(image) = emoji.image(name) {
                crate::image_store::paint_image_contain(scene, transform, square, &image, 0.0);
            }
        }
    }
}

impl crate::text_render::TextRenderer {
    /// `text`, a label in `style` (already at the renderer's scale), laid
    /// out with its workspace emoji when it writes any: such a label is
    /// shaped as attributed runs, measured and painted from the same
    /// layout, instead of the cached plain glyph runs every other label
    /// keeps. `None` for a label that writes none, the usual case.
    pub(crate) fn emoji_label(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &crate::text_render::TextStyle,
    ) -> Option<RichTextLayout> {
        if !text.contains(':') {
            return None;
        }
        let spans = split_spans_in_window(vec![RichSpan::new(text)]);
        if !spans.iter().any(|span| span.workspace_emoji) {
            return None;
        }
        let align = match style.align {
            crate::text_render::ParagraphAlign::Start => super::TextAlign::Left,
            crate::text_render::ParagraphAlign::Center => super::TextAlign::Center,
            crate::text_render::ParagraphAlign::End => super::TextAlign::Right,
            crate::text_render::ParagraphAlign::Justified => super::TextAlign::Justified,
        };
        Some(RichTextLayout::shape_styled(
            &mut self.font_system,
            &spans,
            style,
            max_width,
            align,
            Vec::new(),
            Vec::new(),
        ))
    }
}

#[cfg(test)]
#[path = "inline_emoji_tests.rs"]
pub(crate) mod tests;
