//! `KeyHint`: drawn keycaps and what they do ("Enter Save", "Esc Cancel"),
//! never a sentence with a separator glyph (`docs/design-language.md`,
//! "Primitives").
//!
//! [`KeyHintsFace`] lays a strip of hints out with a `sibling` gap between
//! them; each hint is its keycaps (the shared keycap painter,
//! [`crate::widget::InputHint`], in the person's keyboard legends), a
//! `related` gap, and its label in `Caption` secondary ink.

use vello::kurbo::{Point, Rect};

use crate::paint::PaintCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::InputHint;

/// One hint: the keys of a chord, as printed on the keyboard, and what
/// pressing them does.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyHint {
    pub keys: Vec<String>,
    pub label: String,
}

impl KeyHint {
    /// A hint for one key or a chord. Key names go through the person's
    /// keyboard legends ([`crate::keycaps::active`]), so `Ctrl` reads
    /// `Strg` on a German keyboard.
    #[must_use]
    pub fn new<'a>(keys: impl IntoIterator<Item = &'a str>, label: impl Into<String>) -> Self {
        let caps = crate::keycaps::active();
        Self {
            keys: keys.into_iter().map(|k| caps.token(k)).collect(),
            label: label.into(),
        }
    }
}

fn label_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption)
}

/// A strip of key hints, borrowed.
#[derive(Clone, Copy, Debug)]
pub struct KeyHintsFace<'a> {
    hints: &'a [KeyHint],
}

impl<'a> KeyHintsFace<'a> {
    #[must_use]
    pub fn new(hints: &'a [KeyHint]) -> Self {
        Self { hints }
    }

    /// The strip's height: one keycap.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        InputHint::keycap_height(theme)
    }

    fn hint_width(hint: &KeyHint, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let caps = InputHint::measure_keycap_labels(&hint.keys, text, theme);
        let label = f64::from(text.measure_style(&hint.label, None, &label_style(theme)).0);
        caps + f64::from(theme.tokens.spacing.rhythm.related) + label
    }

    /// The strip's natural width.
    #[must_use]
    pub fn width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let widths: f64 = self
            .hints
            .iter()
            .map(|h| Self::hint_width(h, text, theme))
            .sum();
        #[allow(
            clippy::cast_precision_loss,
            reason = "a strip holds a handful of hints"
        )]
        let gaps = self.hints.len().saturating_sub(1) as f64 * gap;
        widths + gaps
    }

    /// Paint the hints that fit in `rect` from its leading edge, centred on
    /// its middle line; a hint that would not fit whole is left out.
    /// Returns the painted width.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) -> f64 {
        let theme = ctx.theme;
        let rhythm = theme.tokens.spacing.rhythm;
        let style = label_style(theme);
        let cap_h = InputHint::keycap_height(theme);
        let line = f64::from(ctx.text.measure_style("Ag", None, &style).1);
        let cy = rect.center().y;
        let mut x = rect.x0;
        for (i, hint) in self.hints.iter().enumerate() {
            let gap = if i == 0 {
                0.0
            } else {
                f64::from(rhythm.sibling)
            };
            if x + gap + Self::hint_width(hint, ctx.text, theme) > rect.x1 {
                break;
            }
            x += gap;
            x += InputHint::paint_keycap_labels_at(
                ctx.scene,
                ctx.transform,
                Point::new(x, cy - cap_h / 2.0),
                &hint.keys,
                ctx.text,
                theme,
            );
            x += f64::from(rhythm.related);
            ctx.text.draw_style(
                ctx.scene,
                &hint.label,
                theme.tokens.colors.text_dim,
                ctx.transform.pre_translate((x, cy - line / 2.0).into()),
                None,
                &style,
            );
            x += f64::from(ctx.text.measure_style(&hint.label, None, &style).0);
        }
        x - rect.x0
    }

    /// What a screen reader says for each hint: "Enter: Save".
    #[must_use]
    pub fn spoken(&self) -> Vec<String> {
        self.hints
            .iter()
            .map(|h| format!("{}: {}", h.keys.join(" + "), h.label))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_are_keycaps_and_labels_with_no_joining_glyph() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let hints = [
            KeyHint::new(["Enter"], "Save"),
            KeyHint::new(["Esc"], "Cancel"),
        ];
        let face = KeyHintsFace::new(&hints);
        let one = KeyHintsFace::new(&hints[..1]).width(&mut text, &theme);
        let both = face.width(&mut text, &theme);
        assert!(both > one * 1.5);
        assert_eq!(face.spoken(), vec!["Enter: Save", "Esc: Cancel"]);
        for hint in &hints {
            assert!(!hint.label.contains('·') && !hint.label.contains('•'));
        }
    }
}
