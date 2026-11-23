//! The hint's pill: lit in the person colour, a soft glow of it around, a few words and the keys,
//! and a notch pointing at where the gesture is done. Small on purpose: it stands beside the
//! gesture without taking any of it away.

use crate::color::{Oklch, contrast_ratio};
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::InputHint;
use crate::widget::floating_surface::PopoverSide;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill, Gradient};

/// Across the pill, either side of its contents.
const PAD_X: f64 = 10.0;
/// Above and below its contents.
const PAD_Y: f64 = 5.0;
/// The pill's corner: small, so a notch fits on either of its sides.
const RADIUS: f64 = 8.0;
/// Between its words and its keys.
const GAP: f64 = 8.0;
/// The notch pointing at the anchor: its width at the pill and its depth.
pub(super) const NOTCH_W: f64 = 12.0;
pub(super) const NOTCH_D: f64 = 6.0;
/// Below this contrast against its ink the pill's colour moves in
/// lightness until it reads (body text, WCAG AA).
const INK_CONTRAST: f64 = 4.5;
const GLOW_BLUR: f32 = 14.0;

/// The colours one pill is drawn in, from the person's colour and the theme.
#[derive(Clone, Copy, Debug)]
pub(super) struct Look {
    /// The pill's colour: the person's, moved in lightness only as far as its words need.
    pub fill: Color,
    /// Lighter, toward the pill's top, where it is lit from.
    pub lit: Color,
    /// Darker, toward its foot.
    pub deep: Color,
    /// Words and keys.
    pub ink: Color,
    /// The halo around it.
    pub glow: Color,
}

impl Look {
    /// The pill for a person whose colour is `person` (as the theme's presence palette paints it).
    pub fn of(person: Color, theme: &Theme) -> Self {
        let colors = &theme.tokens.colors;
        let ink = colors.surfaces.content;
        let source = Oklch::from_color(person);
        let ink_l = Oklch::from_color(ink).l;
        let away = if ink_l < 0.5 { 1.0 } else { 0.0 };
        let at = |l: f32| {
            Oklch {
                l: l.clamp(0.0, 1.0),
                ..source
            }
            .to_color()
        };
        let mut l = source.l;
        for step in 1..=40_u8 {
            if contrast_ratio(at(l), ink) >= INK_CONTRAST {
                break;
            }
            l = source.l + (away - source.l) * f32::from(step) / 40.0;
        }
        let dark = colors.palette.dark;
        Self {
            fill: at(l),
            lit: at(l + 0.05),
            deep: at(l - 0.04),
            ink,
            glow: at(l).multiply_alpha(if dark { 0.6 } else { 0.5 }),
        }
    }
}

/// The pill's contents, for measuring and painting.
pub(super) struct Contents<'a> {
    pub words: &'a str,
    pub keys: &'a [String],
}

fn words_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Body).with_weight(TextWeight::Semibold)
}

/// The pill's size for `contents`: its words and keys a few pixels apart, one line tall.
pub(super) fn size(contents: &Contents<'_>, text: &mut TextRenderer, theme: &Theme) -> (f64, f64) {
    let (words_w, words_h) = text.measure_style(contents.words, None, &words_style(theme));
    let keys_w = if contents.keys.is_empty() {
        0.0
    } else {
        GAP + InputHint::measure_keycap_labels(contents.keys, text, theme)
    };
    let words = f64::from(words_w).ceil() + keys_w;
    let line = f64::from(words_h).max(InputHint::keycap_height(theme));
    (words + 2.0 * PAD_X, line + 2.0 * PAD_Y)
}

/// The corner a pill of `rect` is drawn with: small, never more than half its side.
pub(super) fn radius(rect: Rect) -> f64 {
    RADIUS.min(rect.width() / 2.0).min(rect.height() / 2.0)
}

/// The point a done pill folds into its dot at: where its leading edge stood.
pub(super) fn dot_centre(rect: Rect) -> Point {
    Point::new(rect.x0 + radius(rect), rect.center().y)
}

/// One painted frame of a pill.
pub(super) struct Frame<'a> {
    pub rect: Rect,
    /// The side of the pill its anchor is on, and where along that side the notch points, `None` for
    /// no notch.
    pub notch: Option<(PopoverSide, f64)>,
    pub look: Look,
    pub contents: Contents<'a>,
    /// Whole-pill opacity.
    pub opacity: f32,
    /// Opacity of what is on the pill, inside its outline.
    pub content_opacity: f32,
    /// The arrival's glow pulse: how far it spread and how strong.
    pub pulse: Option<(f64, f32)>,
}

/// The pill's outline with its notch, one closed path: the notch grows out of the edge it sits on, so
/// the outline's stroke runs round it.
pub(super) fn outline(rect: Rect, notch: Option<(PopoverSide, f64)>) -> BezPath {
    let (x0, y0, x1, y1) = (rect.x0, rect.y0, rect.x1, rect.y1);
    let r = radius(rect);
    let k = 0.552_284_8 * r;
    let half = NOTCH_W / 2.0;
    let along_x = |at: f64| at.clamp(x0 + r + half, (x1 - r - half).max(x0 + r + half));
    let along_y = |at: f64| at.clamp(y0 + r + half, (y1 - r - half).max(y0 + r + half));
    let fits_x = rect.width() >= 2.0 * r + NOTCH_W;
    let fits_y = rect.height() >= 2.0 * r + NOTCH_W;
    let mut path = BezPath::new();
    path.move_to((x0 + r, y0));
    if let Some((PopoverSide::Above, at)) = notch.filter(|_| fits_x) {
        let x = along_x(at);
        path.line_to((x - half, y0));
        path.quad_to((x - 2.5, y0 - NOTCH_D + 1.5), (x, y0 - NOTCH_D));
        path.quad_to((x + 2.5, y0 - NOTCH_D + 1.5), (x + half, y0));
    }
    path.line_to((x1 - r, y0));
    path.curve_to((x1 - r + k, y0), (x1, y0 + r - k), (x1, y0 + r));
    if let Some((PopoverSide::Right, at)) = notch.filter(|_| fits_y) {
        let y = along_y(at);
        path.line_to((x1, y - half));
        path.quad_to((x1 + NOTCH_D - 1.5, y - 2.5), (x1 + NOTCH_D, y));
        path.quad_to((x1 + NOTCH_D - 1.5, y + 2.5), (x1, y + half));
    }
    path.line_to((x1, y1 - r));
    path.curve_to((x1, y1 - r + k), (x1 - r + k, y1), (x1 - r, y1));
    if let Some((PopoverSide::Below, at)) = notch.filter(|_| fits_x) {
        let x = along_x(at);
        path.line_to((x + half, y1));
        path.quad_to((x + 2.5, y1 + NOTCH_D - 1.5), (x, y1 + NOTCH_D));
        path.quad_to((x - 2.5, y1 + NOTCH_D - 1.5), (x - half, y1));
    }
    path.line_to((x0 + r, y1));
    path.curve_to((x0 + r - k, y1), (x0, y1 - r + k), (x0, y1 - r));
    if let Some((PopoverSide::Left, at)) = notch.filter(|_| fits_y) {
        let y = along_y(at);
        path.line_to((x0, y + half));
        path.quad_to((x0 - NOTCH_D + 1.5, y + 2.5), (x0 - NOTCH_D, y));
        path.quad_to((x0 - NOTCH_D + 1.5, y - 2.5), (x0, y - half));
    }
    path.line_to((x0, y0 + r));
    path.curve_to((x0, y0 + r - k), (x0 + r - k, y0), (x0 + r, y0));
    path.close_path();
    path
}

/// Paint one frame of the pill.
pub(super) fn paint(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    frame: &Frame<'_>,
) {
    let rect = frame.rect;
    if frame.opacity <= 0.0 || rect.is_zero_area() {
        return;
    }
    let look = frame.look;
    let shape = outline(rect, frame.notch);
    let radius = radius(rect);
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        frame.opacity,
        transform,
        &rect.inflate(80.0, 80.0),
    );
    // The halo in the person's colour, and a contact shadow at the foot that holds the pill off
    // light grounds.
    scene.draw_blurred_rounded_rect(
        transform,
        rect.inflate(2.0, 2.0),
        look.glow,
        radius + 2.0,
        crate::surface::std_dev_for(GLOW_BLUR),
    );
    crate::surface::shadow(
        scene,
        transform,
        rect,
        radius,
        &theme.tokens.elevation.contact,
    );
    if let Some((spread, strength)) = frame.pulse {
        pulse(
            scene,
            transform,
            rect,
            radius,
            &look,
            (spread, strength),
            false,
        );
    }
    let gradient = Gradient::new_linear((rect.x0, rect.y0), (rect.x0, rect.y1))
        .with_stops([look.lit, look.fill, look.deep].as_slice());
    scene.fill(Fill::NonZero, transform, &gradient, None, &shape);
    scene.push_clip_layer(Fill::NonZero, transform, &shape);
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        look.lit.multiply_alpha(0.9),
        None,
        &RoundedRect::from_rect(rect.inset(-0.5), radius - 0.5),
    );
    scene.pop_layer();
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        look.deep.multiply_alpha(0.5),
        None,
        &shape,
    );
    if frame.content_opacity > 0.0 {
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            frame.content_opacity,
            transform,
            &shape,
        );
        paint_contents(scene, transform, text, theme, frame);
        scene.pop_layer();
    }
    scene.pop_layer();
}

fn paint_contents(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    frame: &Frame<'_>,
) {
    let rect = frame.rect;
    let look = frame.look;
    let style = words_style(theme);
    let (words_w, words_h) = text.measure_style(frame.contents.words, None, &style);
    text.draw_style(
        scene,
        frame.contents.words,
        look.ink,
        transform
            * Affine::translate((rect.x0 + PAD_X, rect.center().y - f64::from(words_h) / 2.0)),
        None,
        &style,
    );
    if !frame.contents.keys.is_empty() {
        let height = InputHint::keycap_height(theme);
        let origin = Point::new(
            rect.x0 + PAD_X + f64::from(words_w).ceil() + GAP,
            rect.center().y - height / 2.0,
        );
        InputHint::paint_keycap_labels_at(
            scene,
            transform,
            origin,
            frame.contents.keys,
            text,
            theme,
        );
    }
}

/// One pulse of light leaving `rect` (corner `radius`): a bloom of the
/// person's colour swelling out behind it and, with `ring`, a bright ring
/// riding its edge, fading as they spread (`(spread, strength)`, spread
/// `0..=1`).
pub(super) fn pulse(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    look: &Look,
    (spread, strength): (f64, f32),
    ring: bool,
) {
    if strength <= 0.0 {
        return;
    }
    let reach = 2.0 + 24.0 * spread;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a blur radius is drawn at f32 precision"
    )]
    let blur = 10.0 + 14.0 * spread as f32;
    scene.draw_blurred_rounded_rect(
        transform,
        rect.inflate(reach, reach),
        look.glow.multiply_alpha(strength),
        radius + reach,
        crate::surface::std_dev_for(blur),
    );
    if ring {
        let out = 1.0 + 16.0 * spread;
        scene.stroke(
            &Stroke::new(1.5),
            transform,
            look.lit.multiply_alpha(strength),
            None,
            &RoundedRect::from_rect(rect.inflate(out, out), radius + out),
        );
    }
}

/// The dot a done pill folds into, at `centre`, `diameter` across.
pub(super) fn dot(scene: &mut Scene, transform: Affine, centre: Point, diameter: f64, look: &Look) {
    scene.draw_blurred_rounded_rect(
        transform,
        Rect::from_center_size(centre, (diameter, diameter)),
        look.glow,
        diameter / 2.0,
        crate::surface::std_dev_for(GLOW_BLUR / 3.0),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        look.fill,
        None,
        &Circle::new(centre, diameter / 2.0),
    );
}
