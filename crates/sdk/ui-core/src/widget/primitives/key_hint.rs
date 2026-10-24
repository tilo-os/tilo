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

pub(crate) fn or_word() -> String {
    widget_format("key-hint-or", &[])
}

fn label_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption)
}

fn word_width(word: &str, text: &mut TextRenderer, theme: &Theme) -> f64 {
    f64::from(text.measure_style(word, None, &label_style(theme)).0)
}

/// A chord's keycaps or a word, as a strip places them.
enum Piece<'a> {
    Keys(&'a [String]),
    Word(String),
}

/// A strip laid out in a width: each piece with its offset from the
/// leading edge and its line, the widest line, and the number of lines.
#[derive(Default)]
struct Placement<'a> {
    pieces: Vec<(Piece<'a>, f64, usize)>,
    width: f64,
    lines: usize,
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

    /// The strip's height on one line: one keycap.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        InputHint::keycap_height(theme)
    }

    fn hint_width(hint: &KeyHint, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let related = f64::from(theme.tokens.spacing.rhythm.related);
        let mut caps = InputHint::measure_keycap_labels(&hint.keys, text, theme);
        if !hint.alternatives.is_empty() {
            let or = word_width(&or_word(), text, theme);
            for keys in &hint.alternatives {
                caps +=
                    related + or + related + InputHint::measure_keycap_labels(keys, text, theme);
            }
        }
        if hint.label.is_empty() {
            // Keys alone (a shortcut's chord): no gap for a missing label.
            return caps;
        }
        caps + related + word_width(&hint.label, text, theme)
    }

    /// Where every chord and word goes in `width`. A chord that would run
    /// past the edge starts a new line after the "or" before it, and a label
    /// after its keys; the first hint is always placed, a later one that
    /// does not fit whole on the line is left out.
    fn place(&self, width: f64, text: &mut TextRenderer, theme: &Theme) -> Placement<'a> {
        let rhythm = theme.tokens.spacing.rhythm;
        let (related, sibling) = (f64::from(rhythm.related), f64::from(rhythm.sibling));
        let mut placement = Placement::default();
        let (mut x, mut line) = (0.0_f64, 0_usize);
        for (i, hint) in self.hints.iter().enumerate() {
            if i > 0 {
                if x + sibling + Self::hint_width(hint, text, theme) > width {
                    break;
                }
                x += sibling;
            }
            for (j, keys) in hint.chords().enumerate() {
                let caps = InputHint::measure_keycap_labels(keys, text, theme);
                if j > 0 {
                    let or = or_word();
                    x += related;
                    let or_width = word_width(&or, text, theme);
                    placement.pieces.push((Piece::Word(or), x, line));
                    x += or_width;
                    if x + related + caps > width {
                        line += 1;
                        x = 0.0;
                    } else {
                        x += related;
                    }
                }
                placement.pieces.push((Piece::Keys(keys), x, line));
                x += caps;
                placement.width = placement.width.max(x);
            }
            if !hint.label.is_empty() {
                let label = word_width(&hint.label, text, theme);
                if x + related + label > width && x > 0.0 {
                    line += 1;
                    x = 0.0;
                } else {
                    x += related;
                }
                placement
                    .pieces
                    .push((Piece::Word(hint.label.clone()), x, line));
                x += label;
                placement.width = placement.width.max(x);
            }
        }
        placement.lines = line + 1;
        placement
    }

    /// The height of `lines` lines of keycaps, a tight gap apart.
    fn lines_height(lines: usize, theme: &Theme) -> f64 {
        let cap = InputHint::keycap_height(theme);
        let gap = f64::from(theme.tokens.spacing.rhythm.tight);
        crate::num::f64_of_usize(lines) * (cap + gap) - gap
    }

    /// The strip's natural width, on one line.
    #[must_use]
    pub fn width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        self.place(f64::INFINITY, text, theme).width
    }

    /// The strip's height laid out in `width`: one keycap per line, a line
    /// for each chord or label that wraps.
    #[must_use]
    pub fn height_in(&self, width: f64, text: &mut TextRenderer, theme: &Theme) -> f64 {
        Self::lines_height(self.place(width, text, theme).lines, theme)
    }

    /// Paint the strip in `rect` from its leading edge, its lines centred
    /// on the rect's middle ([`Self::height_in`] is the room they need).
    /// Returns the painted width.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) -> f64 {
        let theme = ctx.theme;
        let placement = self.place(rect.width(), ctx.text, theme);
        let cap_h = InputHint::keycap_height(theme);
        let pitch = cap_h + f64::from(theme.tokens.spacing.rhythm.tight);
        let line = f64::from(ctx.text.measure_style("Ag", None, &label_style(theme)).1);
        let top = rect.center().y - Self::lines_height(placement.lines, theme) / 2.0;
        for (piece, x, row) in &placement.pieces {
            let y = top + crate::num::f64_of_usize(*row) * pitch;
            match piece {
                Piece::Keys(keys) => {
                    InputHint::paint_keycap_labels_at(
                        ctx.scene,
                        ctx.transform,
                        Point::new(rect.x0 + x, y),
                        keys,
                        ctx.text,
                        theme,
                    );
                }
                Piece::Word(word) => {
                    Self::paint_word(ctx, word, Point::new(rect.x0 + x, y + (cap_h - line) / 2.0));
                }
            }
        }
        placement.width
    }

    /// A word in `Caption` secondary ink at `origin`.
    fn paint_word(ctx: &mut PaintCtx<'_>, word: &str, origin: Point) {
        ctx.text.draw_style(
            ctx.scene,
            word,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform.pre_translate(origin.to_vec2()),
            None,
            &label_style(ctx.theme),
        );
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
        // Measured in the width it is given: chords wrap after "or".
        let style = taffy::Style {
            size: taffy::Size::auto(),
            min_size: taffy::Size {
                width: taffy::Dimension::length(0.0),
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(theme) = self.theme.as_ref() else {
            return taffy::Size::ZERO;
        };
        let face = KeyHintsFace::new(&self.hints);
        let natural = face.width(text, theme).ceil();
        let width = match (known.width, available.width) {
            (Some(width), _) => f64::from(width),
            (None, taffy::AvailableSpace::Definite(room)) => natural.min(f64::from(room)),
            // Every chord on its own line: the widest one.
            (None, taffy::AvailableSpace::MinContent) => face.place(0.0, text, theme).width.ceil(),
            (None, taffy::AvailableSpace::MaxContent) => natural,
        };
        taffy::Size {
            width: crate::num::f32_of_f64(width),
            height: known
                .height
                .unwrap_or_else(|| crate::num::f32_of_f64(face.height_in(width, text, theme))),
        }
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

    #[test]
    fn a_row_narrower_than_its_alternatives_wraps_after_or_and_stays_inside() {
        crate::i18n::I18nManager::init("en-US");
        crate::i18n::I18nManager::register_widget_messages();
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let keys = |names: &[&str]| names.iter().map(|k| (*k).to_owned()).collect();
        let tabs = [KeyHint::any_of(
            vec![
                keys(&["Super", "Ctrl", "Shift", "Tab"]),
                keys(&["Super", "Ctrl", "Tab"]),
            ],
            "",
        )
        .unwrap()];
        let face = KeyHintsFace::new(&tabs);
        let natural = face.width(&mut text, &theme);
        let cap = InputHint::keycap_height(&theme);
        assert!((face.height_in(natural, &mut text, &theme) - cap).abs() < 0.01);

        let width = natural - 10.0;
        let placed = face.place(width, &mut text, &theme);
        assert_eq!(placed.lines, 2, "the second chord moves to its own line");
        assert!(placed.width <= width, "{} in {width}", placed.width);
        let or_line = placed.pieces.iter().find_map(|(piece, _, line)| {
            matches!(piece, Piece::Word(w) if *w == or_word()).then_some(*line)
        });
        assert_eq!(or_line, Some(0), "the or ends the first line");
        let (_, x, line) = placed.pieces.last().unwrap();
        assert!(matches!(placed.pieces.last().unwrap().0, Piece::Keys(_)));
        assert_eq!(
            (*x, *line),
            (0.0, 1),
            "the second chord starts the next line"
        );
        let tight = f64::from(theme.tokens.spacing.rhythm.tight);
        assert!((face.height_in(width, &mut text, &theme) - (2.0 * cap + tight)).abs() < 0.01);
    }
}
