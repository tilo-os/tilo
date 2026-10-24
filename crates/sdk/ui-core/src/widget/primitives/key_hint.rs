//! `KeyHint`: drawn keycaps and what they do ("Enter Save", "Esc Cancel"),
//! never a sentence with a separator glyph (`docs/design-language.md`,
//! "Primitives").
//!
//! [`KeyHintsFace`] lays a strip of hints out with a `sibling` gap between
//! them; each hint is its keycaps (the shared keycap painter,
//! [`crate::widget::InputHint`], in the person's keyboard legends), a
//! `related` gap, and its label in `Caption` secondary ink. A hint with
//! alternatives (two chords that do the same) shows each further chord after
//! the catalogue's "or", so they never read as one long chord.

use vello::kurbo::{Point, Rect};

use crate::i18n::widget_format;
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
    /// Other chords that do the same, each shown after "or".
    pub alternatives: Vec<Vec<String>>,
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
            alternatives: Vec::new(),
            label: label.into(),
        }
    }

    /// A hint for chords that each do the same thing, already in keycap
    /// legends: the first, then every other after "or". `None` for no chord.
    #[must_use]
    pub fn any_of(chords: Vec<Vec<String>>, label: impl Into<String>) -> Option<Self> {
        let mut chords = chords.into_iter();
        let keys = chords.next()?;
        Some(Self {
            keys,
            alternatives: chords.collect(),
            label: label.into(),
        })
    }

    /// The chords in order, the first and then its alternatives.
    fn chords(&self) -> impl Iterator<Item = &[String]> {
        std::iter::once(self.keys.as_slice()).chain(self.alternatives.iter().map(Vec::as_slice))
    }

    /// What a screen reader says: "Enter: Save", or for keys alone
    /// "Alt + F4 or Super + W".
    fn spoken(&self) -> String {
        let or = format!(" {} ", or_word());
        let keys = self
            .chords()
            .map(|keys| keys.join(" + "))
            .collect::<Vec<_>>()
            .join(&or);
        if self.label.is_empty() {
            keys
        } else {
            format!("{keys}: {}", self.label)
        }
    }
}

fn or_word() -> String {
    widget_format("key-hint-or", &[])
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
        let related = f64::from(theme.tokens.spacing.rhythm.related);
        let mut caps = InputHint::measure_keycap_labels(&hint.keys, text, theme);
        if !hint.alternatives.is_empty() {
            let or = f64::from(text.measure_style(&or_word(), None, &label_style(theme)).0);
            for keys in &hint.alternatives {
                caps +=
                    related + or + related + InputHint::measure_keycap_labels(keys, text, theme);
            }
        }
        if hint.label.is_empty() {
            // Keys alone (a shortcut's chord): no gap for a missing label.
            return caps;
        }
        let label = f64::from(text.measure_style(&hint.label, None, &label_style(theme)).0);
        caps + related + label
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
            for (j, keys) in hint.chords().enumerate() {
                if j > 0 {
                    x += f64::from(rhythm.related);
                    x += Self::paint_word(ctx, &or_word(), Point::new(x, cy - line / 2.0));
                    x += f64::from(rhythm.related);
                }
                x += InputHint::paint_keycap_labels_at(
                    ctx.scene,
                    ctx.transform,
                    Point::new(x, cy - cap_h / 2.0),
                    keys,
                    ctx.text,
                    theme,
                );
            }
            if hint.label.is_empty() {
                continue;
            }
            x += f64::from(rhythm.related);
            x += Self::paint_word(ctx, &hint.label, Point::new(x, cy - line / 2.0));
        }
        x - rect.x0
    }

    /// A word in `Caption` secondary ink at `origin`; returns its width.
    fn paint_word(ctx: &mut PaintCtx<'_>, word: &str, origin: Point) -> f64 {
        let style = label_style(ctx.theme);
        ctx.text.draw_style(
            ctx.scene,
            word,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform.pre_translate(origin.to_vec2()),
            None,
            &style,
        );
        f64::from(ctx.text.measure_style(word, None, &style).0)
    }

    /// What a screen reader says for each hint: "Enter: Save".
    #[must_use]
    pub fn spoken(&self) -> Vec<String> {
        self.hints.iter().map(KeyHint::spoken).collect()
    }
}

/// A retained strip of key hints: a chord's drawn keycaps on a row's
/// trailing edge (a shortcut that cannot be changed), or several chords
/// side by side, a `sibling` gap apart. Never a joined string.
pub struct KeyHints {
    hints: Vec<KeyHint>,
    rect: Rect,
    /// Not yet laid out in the theme last seen, or not yet drawn.
    dirty: bool,
    theme: Option<Theme>,
}

impl KeyHints {
    #[must_use]
    pub fn new(hints: Vec<KeyHint>) -> Self {
        Self {
            hints,
            rect: Rect::ZERO,
            dirty: true,
            theme: None,
        }
    }
}

impl crate::widget::Widget for KeyHints {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> taffy::NodeId {
        use crate::widget::TaffyTreeExt as _;
        if self.theme.as_ref() != Some(theme) {
            self.theme = Some(theme.clone());
            self.dirty = true;
        }
        let width = crate::text_render::with_layout_renderer(theme.text_scale, |text| {
            KeyHintsFace::new(&self.hints).width(text, theme)
        });
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(crate::num::f32_of_f64(width.ceil())),
                height: taffy::Dimension::length(crate::num::f32_of_f64(KeyHintsFace::height(
                    theme,
                ))),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    // Its keys never change: it is measured again only in a new theme.
    fn is_layout_dirty(&self) -> bool {
        self.dirty
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        // A little room past the measured width so rounding never drops
        // the last hint.
        let rect = self.rect.inflate(1.0, 0.0);
        KeyHintsFace::new(&self.hints).paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
        );
    }

    fn name(&self) -> &'static str {
        "KeyHints"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let spoken: Vec<String> = self.hints.iter().map(KeyHint::spoken).collect();
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::StaticText)
                .with_name(spoken.join(", "))
                .with_bounds(self.rect),
        )
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

    #[test]
    fn alternative_chords_are_joined_by_the_catalogue_or() {
        crate::i18n::I18nManager::init("en-US");
        crate::i18n::I18nManager::register_widget_messages();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let alt_f4 = vec!["Alt".to_owned(), "F4".to_owned()];
        let super_w = vec!["Super".to_owned(), "W".to_owned()];
        let either = [KeyHint::any_of(vec![alt_f4.clone(), super_w.clone()], "").unwrap()];
        assert_eq!(either[0].keys, alt_f4);
        assert_eq!(either[0].alternatives, vec![super_w.clone()]);
        let or = or_word();
        assert_eq!(or, "or");
        assert_eq!(
            KeyHintsFace::new(&either).spoken(),
            vec![format!("Alt + F4 {or} Super + W")]
        );

        // The "or" takes room between the chords: wider than both chords
        // side by side as two hints a sibling gap apart.
        let side_by_side = [
            KeyHint::any_of(vec![alt_f4], "").unwrap(),
            KeyHint::any_of(vec![super_w], "").unwrap(),
        ];
        let rhythm = theme.tokens.spacing.rhythm;
        let or_width = f64::from(text.measure_style(&or, None, &label_style(&theme)).0);
        let joined = KeyHintsFace::new(&either).width(&mut text, &theme);
        let apart = KeyHintsFace::new(&side_by_side).width(&mut text, &theme);
        let expected =
            apart - f64::from(rhythm.sibling) + 2.0 * f64::from(rhythm.related) + or_width;
        assert!((joined - expected).abs() < 0.01, "{joined} vs {expected}");
        assert!(KeyHint::any_of(Vec::new(), "").is_none());
    }
}
