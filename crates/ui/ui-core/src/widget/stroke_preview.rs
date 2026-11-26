//! `StrokePreview`: the mark a brush, its size and a pigment will paint, drawn live as a sample
//! stroke, so a person sees what they are about to lay down before they touch the canvas.
//!
//! The sample is one S-shaped stroke across the preview's own area, in the brush's character: a
//! pen thins at both ends, a marker is flat and see-through where it crosses itself, charcoal breaks
//! up, a broad nib is thick across its angle and thin along it, an eraser leaves the paper's checker
//! showing. A small chip at the stroke's end is the pigment itself, as a wet blob. A changed brush,
//! size or colour draws the stroke in again along its path (`motion::Disclose`'s extent), and the
//! preview asks for frames only while it does.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{
    Affine, BezPath, Cap, Circle, CubicBez, Join, ParamCurve, ParamCurveDeriv, Point, Rect, Stroke,
    Vec2,
};
use vello::peniko::{Color, Fill};

use crate::color_compat::ColorCompat;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

#[path = "stroke_preview_component.rs"]
mod component;
pub use component::StrokePreviewProps;

/// How a brush lays paint down, by the name an artwork's marks keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BrushCharacter {
    /// The round ink brush: one even band of colour.
    #[default]
    Round,
    /// A fountain pen: the pressure tapers the stroke at both ends.
    Pen,
    /// Graphite: thin, grainy and a little see-through.
    Pencil,
    /// A felt marker: flat ends, see-through, and darker where it crosses itself.
    Marker,
    /// A soft spray: a widening, fading cloud.
    Airbrush,
    /// Charcoal: dry, ragged, catching the paper unevenly.
    Charcoal,
    /// A broad nib held at an angle: thick across it, thin along it.
    Calligraphy,
    /// A rubber: it takes paint away and leaves the paper showing.
    Eraser,
    /// The eraser that rubs out whole artwork cells.
    PixelEraser,
    /// Whole square artwork cells, with no softened edge.
    Pixel,
}

impl BrushCharacter {
    /// Every character, in the order the names run.
    pub const ALL: [Self; 10] = [
        Self::Round,
        Self::Pen,
        Self::Pencil,
        Self::Marker,
        Self::Airbrush,
        Self::Charcoal,
        Self::Calligraphy,
        Self::Eraser,
        Self::Pixel,
        Self::PixelEraser,
    ];

    /// The name a mark keeps for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Round => "round",
            Self::Pen => "pen",
            Self::Pencil => "pencil",
            Self::Marker => "marker",
            Self::Airbrush => "airbrush",
            Self::Charcoal => "charcoal",
            Self::Calligraphy => "calligraphy",
            Self::Eraser => "eraser",
            Self::Pixel => "pixel",
            Self::PixelEraser => "pixel-eraser",
        }
    }

    /// The character `name` names; an unknown name paints like the round brush.
    #[must_use]
    pub fn parse(name: &str) -> Self {
        let wanted = name.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|character| character.name() == wanted)
            .unwrap_or_default()
    }

    /// Whether it takes paint away rather than laying it.
    #[must_use]
    pub const fn erases(self) -> bool {
        matches!(self, Self::Eraser | Self::PixelEraser)
    }
}

/// The preview's preferred logical size: a sample long and tall enough to read a brush by.
pub const PREFERRED_WIDTH: f64 = 160.0;
/// See [`PREFERRED_WIDTH`].
pub const PREFERRED_HEIGHT: f64 = 40.0;

/// How far the sample keeps from the preview's own edge, so a wide brush stays inside it.
const MARGIN: f64 = 7.0;
/// The widest a brush is drawn: a share of the preview's height, clamped in [`StrokePreviewFace`].
const WIDEST_SHARE: f64 = 0.62;
/// The width a brush paints at when it names no size.
const DEFAULT_SIZE: f64 = 6.0;
/// How finely the sample's path is measured: the points a stroke is painted from.
const SEGMENTS: usize = 72;
/// How strong a wash of the paper's own ink the checker under an eraser is drawn at, in `u8` alpha.
const CELL_WASH_ALPHA: u8 = 30;
/// The angle a calligraphy nib is held at, in radians (45°).
const NIB_ANGLE: f64 = std::f64::consts::FRAC_PI_4;

/// One point of the sample's path: where it is, which way it runs there, and how far along it is.
#[derive(Clone, Copy, Debug)]
struct Sample {
    point: Point,
    tangent: Vec2,
    /// `0..=1` along the whole path, never just the revealed part.
    along: f64,
}

impl Sample {
    /// The unit normal, the side its width is spread to.
    fn normal(&self) -> Vec2 {
        let length = self.tangent.hypot().max(1e-6);
        Vec2::new(-self.tangent.y / length, self.tangent.x / length)
    }
}

/// A repeatable `0..=1` from `seed`: the grain of a pencil or charcoal line, the same every frame.
fn noise(seed: u32) -> f64 {
    let mixed = seed.wrapping_mul(2_654_435_761).rotate_left(13) ^ 0x85EB_CA6B;
    f64::from(mixed >> 8) / f64::from(0x00FF_FFFF_u32)
}

/// The sample stroke in `area`: an S that starts at its top left and ends at its bottom right.
fn curve_in(area: Rect) -> CubicBez {
    let centre = area.center().y;
    let rise = (area.height() / 2.0) * 0.78;
    let reach = area.width() * 0.36;
    CubicBez::new(
        Point::new(area.x0, centre - rise),
        Point::new(area.x0 + reach, centre - rise),
        Point::new(area.x1 - reach, centre + rise),
        Point::new(area.x1, centre + rise),
    )
}

/// The points of `curve` revealed `reveal` of the way in, from its start.
fn samples_of(curve: CubicBez, reveal: f64) -> Vec<Sample> {
    let reveal = reveal.clamp(0.0, 1.0);
    if reveal <= 0.0 {
        return Vec::new();
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a count of at most SEGMENTS points"
    )]
    let drawn = ((SEGMENTS as f64) * reveal).ceil().max(1.0) as usize;
    #[allow(
        clippy::cast_precision_loss,
        reason = "a count of at most SEGMENTS points is exact in f64"
    )]
    let last = drawn as f64;
    let derivative = curve.deriv();
    (0..=drawn)
        .map(|index| {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a count of at most SEGMENTS points is exact in f64"
            )]
            let at = index as f64 / last;
            let along = reveal * at;
            Sample {
                point: curve.eval(along),
                tangent: derivative.eval(along).to_vec2(),
                along,
            }
        })
        .collect()
}

/// The polyline through `samples`.
fn path_of(samples: &[Sample]) -> BezPath {
    let mut path = BezPath::new();
    let mut points = samples.iter();
    if let Some(first) = points.next() {
        path.move_to(first.point);
        for sample in points {
            path.line_to(sample.point);
        }
    }
    path
}

/// The area a variable-width stroke covers: `width` of `samples` laid out on both sides of the path
/// and closed around it.
fn ribbon(samples: &[Sample], width: &dyn Fn(&Sample) -> f64) -> BezPath {
    let mut left = Vec::with_capacity(samples.len());
    let mut right = Vec::with_capacity(samples.len());
    for sample in samples {
        let normal = sample.normal();
        let half = (width(sample) / 2.0).max(0.0);
        left.push(sample.point + normal * half);
        right.push(sample.point - normal * half);
    }
    let mut path = BezPath::new();
    let mut leading = left.iter();
    if let Some(first) = leading.next() {
        path.move_to(*first);
        for point in leading {
            path.line_to(*point);
        }
    }
    for point in right.iter().rev() {
        path.line_to(*point);
    }
    path.close_path();
    path
}

/// The runs of `samples` a grainy medium lays down: each point kept by the noise at `seed`, offset
/// across the path by `offset`, and broken where the noise leaves a gap.
fn grainy_runs(samples: &[Sample], offset: f64, seed: u32) -> Vec<Vec<Sample>> {
    let mut runs: Vec<Vec<Sample>> = Vec::new();
    let mut run: Vec<Sample> = Vec::new();
    for (index, sample) in samples.iter().enumerate() {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "an index into a path of at most SEGMENTS points"
        )]
        let grain = noise((index as u32).wrapping_mul(7).wrapping_add(seed));
        if grain > 0.24 {
            let normal = sample.normal();
            run.push(Sample {
                point: sample.point + normal * offset * (0.5 + grain),
                ..*sample
            });
        } else if !run.is_empty() {
            if run.len() >= 2 {
                runs.push(std::mem::take(&mut run));
            } else {
                run.clear();
            }
        }
    }
    if run.len() >= 2 {
        runs.push(run);
    }
    runs
}

/// The samples of `samples` between `from` and `to` along the path.
fn between(samples: &[Sample], from: f64, to: f64) -> Vec<Sample> {
    samples
        .iter()
        .copied()
        .filter(|sample| (from..=to).contains(&sample.along))
        .collect()
}

/// The sample stroke as it is painted into a rect: what the brush, its size and the pigment make of
/// it, `reveal` of the way drawn in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokePreviewFace {
    brush: BrushCharacter,
    size: f64,
    colour: Color,
    reveal: f64,
}

impl StrokePreviewFace {
    /// `brush` at `size` artwork units in `colour`.
    #[must_use]
    pub const fn new(brush: BrushCharacter, size: f64, colour: Color) -> Self {
        Self {
            brush,
            size,
            colour,
            reveal: 1.0,
        }
    }

    /// How much of the stroke is drawn in, `0..=1`.
    #[must_use]
    pub fn revealed(mut self, reveal: f64) -> Self {
        self.reveal = reveal.clamp(0.0, 1.0);
        self
    }

    /// The width the brush is drawn at in a preview `height` tall: its own size in artwork units,
    /// clamped so a wide brush stays a sample rather than a slab.
    #[must_use]
    pub fn drawn_width(&self, height: f64) -> f64 {
        let size = if self.size.is_finite() && self.size > 0.0 {
            self.size
        } else {
            DEFAULT_SIZE
        };
        size.clamp(0.75, (height * WIDEST_SHARE).max(0.75))
    }

    /// Paint the preview into `rect` on the paper-like surface, its stroke in the brush's character
    /// and its pigment chip at the stroke's end.
    pub fn paint(&self, scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.control);
        crate::surface::card(
            scene,
            transform,
            rect,
            radius,
            theme.tokens.colors.surfaces.raised,
        );
        let area = rect.inset(MARGIN);
        if area.width() <= 1.0 || area.height() <= 1.0 {
            return;
        }
        let width = self.drawn_width(rect.height());
        let curve = curve_in(area);
        let samples = samples_of(curve, self.reveal);
        crate::surface::push_card_clip(scene, transform, rect, radius);
        self.paint_mark(scene, transform, theme, &samples, width);
        self.paint_chip(scene, transform, curve.eval(1.0), width);
        scene.pop_layer();
    }

    /// The stroke itself, in the brush's character.
    fn paint_mark(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        samples: &[Sample],
        width: f64,
    ) {
        if samples.len() < 2 {
            return;
        }
        let colour = self.colour;
        match self.brush {
            BrushCharacter::Round => {
                let stroke = Stroke::new(width)
                    .with_caps(Cap::Round)
                    .with_join(Join::Round);
                scene.stroke(&stroke, transform, colour, None, &path_of(samples));
            }
            BrushCharacter::Pen => {
                // Ink from a nib: full in the middle of the stroke and thinning to both ends.
                let pressure = |sample: &Sample| {
                    let swell = (std::f64::consts::PI * sample.along).sin();
                    width * (0.35 + 0.65 * swell)
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colour,
                    None,
                    &ribbon(samples, &pressure),
                );
            }
            BrushCharacter::Pencil => {
                // Graphite: a few thin strands catching the paper's tooth unevenly.
                let thin = Stroke::new((width * 0.42).max(0.6))
                    .with_caps(Cap::Round)
                    .with_join(Join::Round);
                let ink = colour.with_alpha_factor(0.62);
                for (strand, offset) in [-0.22, 0.0, 0.18].into_iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation, reason = "one of three strands")]
                    let seed = strand as u32 * 41 + 7;
                    for run in grainy_runs(samples, width * offset, seed) {
                        scene.stroke(&thin, transform, ink, None, &path_of(&run));
                    }
                }
            }
            BrushCharacter::Marker => {
                // Felt: flat ends, see-through, and darker where the stroke crosses itself.
                let felt = Stroke::new(width)
                    .with_caps(Cap::Butt)
                    .with_join(Join::Round);
                let ink = colour.with_alpha_factor(0.52);
                scene.stroke(&felt, transform, ink, None, &path_of(samples));
                let retraced = between(samples, 0.30, 0.70);
                if retraced.len() >= 2 {
                    scene.stroke(&felt, transform, ink, None, &path_of(&retraced));
                }
            }
            BrushCharacter::Airbrush => {
                // Spray: widening, fading bands, so the edge of the mark is soft.
                for (share, alpha) in [
                    (0.28, 0.18),
                    (0.52, 0.15),
                    (0.76, 0.12),
                    (1.0, 0.10),
                    (1.3, 0.07),
                ] {
                    let stroke = Stroke::new(width * share)
                        .with_caps(Cap::Round)
                        .with_join(Join::Round);
                    scene.stroke(
                        &stroke,
                        transform,
                        colour.with_alpha_factor(alpha),
                        None,
                        &path_of(samples),
                    );
                }
            }
            BrushCharacter::Charcoal => {
                // Dry and ragged: a rough body with strands breaking off its edge.
                let body = Stroke::new(width * 0.5)
                    .with_caps(Cap::Round)
                    .with_join(Join::Round);
                scene.stroke(
                    &body,
                    transform,
                    colour.with_alpha_factor(0.22),
                    None,
                    &path_of(samples),
                );
                let strand = Stroke::new((width * 0.4).max(0.6))
                    .with_caps(Cap::Round)
                    .with_join(Join::Round);
                for (edge, offset) in [-0.46, -0.12, 0.16, 0.44].into_iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation, reason = "one of four strands")]
                    let seed = edge as u32 * 97 + 13;
                    let ink = colour.with_alpha_factor(0.42);
                    for run in grainy_runs(samples, width * offset, seed) {
                        scene.stroke(&strand, transform, ink, None, &path_of(&run));
                    }
                }
            }
            BrushCharacter::Calligraphy => {
                // A flat nib at 45°: thick where the stroke runs across it, thin along it.
                let nib = |sample: &Sample| {
                    let angle = sample.tangent.y.atan2(sample.tangent.x);
                    let across = (angle - NIB_ANGLE).sin().abs();
                    width * (0.14 + 0.86 * across)
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colour,
                    None,
                    &ribbon(samples, &nib),
                );
            }
            BrushCharacter::Eraser | BrushCharacter::PixelEraser => {
                let cells = if self.brush == BrushCharacter::PixelEraser {
                    1.6
                } else {
                    0.95
                };
                self.paint_erased(scene, transform, theme, samples, width, cells);
            }
            BrushCharacter::Pixel => {
                // Whole square cells: the mark steps rather than curves.
                let cell = width.max(2.0);
                let step = (samples.len() / 26).max(1);
                for sample in samples.iter().step_by(step) {
                    let snapped = Point::new(
                        cell * (sample.point.x / cell).round(),
                        cell * (sample.point.y / cell).round(),
                    );
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        colour,
                        None,
                        &Rect::from_center_size(snapped, (cell, cell)),
                    );
                }
            }
        }
    }

    /// What an eraser leaves: the paint it rubs out, faint, under the checker of the paper showing
    /// through, in cells of `cell_scale` brush widths.
    fn paint_erased(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        samples: &[Sample],
        width: f64,
        cell_scale: f64,
    ) {
        let under = Stroke::new(width * 0.9).with_caps(Cap::Round);
        scene.stroke(
            &under,
            transform,
            self.colour.with_alpha_factor(0.20),
            None,
            &path_of(samples),
        );
        let cell = (width * cell_scale).max(3.0);
        // The paper the eraser uncovers, and a wash of its own ink: the checker a person reads as
        // "nothing here", whichever side of the light/dark line the theme falls on.
        let light = theme.tokens.colors.surfaces.raised;
        let dark = crate::tokens::colors::Colors::hairline_on(light, CELL_WASH_ALPHA);
        let reach = width * 0.55;
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for sample in samples {
            x0 = x0.min(sample.point.x);
            y0 = y0.min(sample.point.y);
            x1 = x1.max(sample.point.x);
            y1 = y1.max(sample.point.y);
        }
        if x0 > x1 || y0 > y1 {
            return;
        }
        let first_column = ((x0 - reach) / cell).floor() as i64;
        let last_column = ((x1 + reach) / cell).ceil() as i64;
        let first_row = ((y0 - reach) / cell).floor() as i64;
        let last_row = ((y1 + reach) / cell).ceil() as i64;
        for row in first_row..=last_row {
            for column in first_column..=last_column {
                let centre = Point::new((column as f64 + 0.5) * cell, (row as f64 + 0.5) * cell);
                let near = samples
                    .iter()
                    .step_by(3)
                    .any(|sample| (sample.point - centre).hypot() <= reach);
                if near {
                    let checker = (column + row) % 2 == 0;
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        if checker { light } else { dark },
                        None,
                        &Rect::from_center_size(centre, (cell, cell)),
                    );
                }
            }
        }
    }

    /// The pigment at the stroke's end, as a wet blob with a catchlight.
    fn paint_chip(&self, scene: &mut Scene, transform: Affine, end: Point, width: f64) {
        let radius = (width * 0.6).clamp(2.5, 6.0);
        let chip = Circle::new(end, radius);
        scene.fill(Fill::NonZero, transform, self.colour, None, &chip);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(self.colour),
            None,
            &chip,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            Color::WHITE.with_alpha_factor(0.45),
            None,
            &Circle::new(
                end + Vec2::new(-radius * 0.34, -radius * 0.34),
                radius * 0.3,
            ),
        );
    }
}

/// The mark a brush, its size and a pigment will paint, drawn as a sample stroke: what a canvas's
/// brush pad shows before a person touches it.
pub struct StrokePreview {
    brush: BrushCharacter,
    size: f64,
    colour: Color,
    label: Option<String>,
    /// How far the sample stroke is drawn in, `0..=1`.
    reveal: crate::motion::Extent,
    /// The motion the last frame was drawn with, so a change taken in place moves as the theme says.
    motion: Option<crate::tokens::EffectiveMotion>,
    rect: Rect,
    dirty: bool,
}

impl StrokePreview {
    /// A preview of `brush` at `size` artwork units in `colour`, drawn whole.
    #[must_use]
    pub fn new(brush: BrushCharacter, size: f64, colour: Color) -> Self {
        let mut reveal = crate::motion::Extent::default();
        reveal.snap(1.0);
        Self {
            brush,
            size,
            colour,
            label: None,
            reveal,
            motion: None,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// The character shown.
    #[must_use]
    pub const fn brush(&self) -> BrushCharacter {
        self.brush
    }

    /// The size shown, in artwork units.
    #[must_use]
    pub const fn size(&self) -> f64 {
        self.size
    }

    /// The pigment shown.
    #[must_use]
    pub const fn colour(&self) -> Color {
        self.colour
    }

    /// The name assistive technology reads; the toolkit's own when it is given none.
    #[must_use]
    pub fn spoken_name(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| crate::i18n::widget_text("stroke-preview-name"))
    }

    /// Name the preview for assistive technology; `None` or empty leaves the toolkit's name.
    pub fn set_accessible_name(&mut self, name: Option<String>) {
        self.label = name.filter(|name| !name.is_empty());
    }

    /// Show `brush` at `size` in `colour`. A change draws the sample stroke in again from its start,
    /// so the person sees what changed; a change that leaves all three alone moves nothing.
    pub fn set_sample(&mut self, brush: BrushCharacter, size: f64, colour: Color) {
        let same_size = self.size.partial_cmp(&size) == Some(std::cmp::Ordering::Equal);
        if self.brush == brush && same_size && self.colour == colour {
            return;
        }
        self.brush = brush;
        self.size = size;
        self.colour = colour;
        self.dirty = true;
        let now = crate::motion::frame_now();
        let motion = self.motion.unwrap_or_else(|| {
            crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), false)
        });
        self.reveal.snap(0.0);
        self.reveal
            .follow(1.0, motion, crate::motion::input_source(), now);
    }

    /// How far the sample stroke is drawn in at `now`, `0..=1`.
    #[must_use]
    pub fn reveal_at(&self, now: crate::time::Instant) -> f64 {
        self.reveal.get(now).clamp(0.0, 1.0)
    }
}

impl Widget for StrokePreview {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The size it prefers, and wider when the layout stretches it: a preview in a row fills the
        // room the row gives it.
        let style = Style {
            size: Size {
                width: Dimension::auto(),
                height: Dimension::length(crate::num::f32_of_f64(PREFERRED_HEIGHT)),
            },
            min_size: Size {
                width: Dimension::length(crate::num::f32_of_f64(PREFERRED_WIDTH)),
                height: Dimension::auto(),
            },
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _text_renderer, theme, mut env) = ctx.parts();
        self.motion = Some(theme.motion());
        let reveal = self.reveal_at(crate::motion::frame_now());
        StrokePreviewFace::new(self.brush, self.size, self.colour)
            .revealed(reveal)
            .paint(scene, transform, theme, self.rect);
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.reveal.is_moving(crate::motion::frame_now())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Frames only while the sample stroke draws itself in.
    fn needs_animation_frame(&self) -> bool {
        self.reveal.is_moving(crate::motion::frame_now())
    }

    fn name(&self) -> &'static str {
        "StrokePreview"
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let character = crate::i18n::widget_text(&format!("stroke-preview-{}", self.brush.name()));
        Some(
            AccessNode::new(Role::Image)
                .with_name(self.spoken_name())
                .with_description(character),
        )
    }
}

/// The pigment `hex` names (`#rrggbb` or `#rrggbbaa`); an ink the previews of any theme read is
/// what a name it cannot read stands for.
fn pigment(hex: &str) -> Color {
    crate::color::parse_hex(hex.trim()).unwrap_or(Color::from_rgb8(0x1d, 0x1b, 0x19))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Described;
    use vello_encoding::DrawTag;

    fn props(brush: &str) -> StrokePreviewProps {
        StrokePreviewProps {
            brush: brush.to_owned(),
            size: 6.0,
            colour: "#c8321e".to_owned(),
            label: None,
        }
    }

    #[test]
    fn a_changed_sample_draws_itself_in_and_then_rests() {
        let _frozen = crate::motion::testing::frozen();
        let now = crate::motion::frame_now();
        let mut preview = StrokePreview::from_props("preview", props("round"));
        assert_eq!(preview.brush(), BrushCharacter::Round);
        assert!(
            !preview.needs_animation_frame(),
            "a preview built for a frame rests as it is"
        );
        assert_eq!(crate::motion::frames_wanted_by(now), Vec::<String>::new());

        // A person choosing another brush in the pad: the sample draws itself in.
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        assert!(preview.set_props(props("charcoal")), "taken in place");
        assert_eq!(preview.brush(), BrushCharacter::Charcoal);
        assert!(
            preview.needs_animation_frame(),
            "the new stroke draws itself in"
        );
        assert!(
            !crate::motion::frames_wanted_by(now).is_empty(),
            "and asks the frame pump for the frames it draws in"
        );
        assert_eq!(preview.reveal_at(now), 0.0, "it starts at the path's start");

        let until = crate::motion::frames_until().expect("the reveal is on its way");
        assert!(until > now);
        crate::motion::frame_drawn(until);
        let _at = crate::motion::pin_frame_at(until);
        assert!(
            !preview.needs_animation_frame(),
            "landed, the preview asks for no more frames"
        );
        assert_eq!(preview.reveal_at(until), 1.0);
        assert_eq!(crate::motion::frames_wanted_by(until), Vec::<String>::new());
    }

    #[test]
    fn a_change_that_is_not_a_persons_hand_lands_at_once() {
        // A key, an action or an agent sets the brush: the sample is there on the next frame, as
        // every result the person did not move something for is (HIG §9.7).
        let _frozen = crate::motion::testing::frozen();
        let now = crate::motion::frame_now();
        let mut preview = StrokePreview::from_props("preview", props("round"));
        crate::motion::note_source(crate::tokens::MotionSource::Action);
        assert!(preview.set_props(props("airbrush")));
        assert!(!preview.needs_animation_frame());
        assert_eq!(preview.reveal_at(now), 1.0);
        assert_eq!(crate::motion::frames_wanted_by(now), Vec::<String>::new());
    }

    #[test]
    fn a_wide_brush_is_drawn_no_wider_than_the_preview_holds() {
        let face = StrokePreviewFace::new(BrushCharacter::Round, 90.0, Color::BLACK);
        assert!((face.drawn_width(40.0) - 40.0 * WIDEST_SHARE).abs() < 1e-9);
        let thin = StrokePreviewFace::new(BrushCharacter::Pencil, 0.02, Color::BLACK);
        assert!(thin.drawn_width(40.0) >= 0.75);
        let unnamed = StrokePreviewFace::new(BrushCharacter::Round, 0.0, Color::BLACK);
        assert!((unnamed.drawn_width(80.0) - DEFAULT_SIZE).abs() < 1e-9);
    }

    #[test]
    fn each_character_paints_the_shear_of_its_mark() {
        let rect = Rect::new(0.0, 0.0, PREFERRED_WIDTH, PREFERRED_HEIGHT);
        let theme = Theme::dark();
        for character in BrushCharacter::ALL {
            let mut whole = Scene::new();
            StrokePreviewFace::new(character, 8.0, Color::from_rgb8(0xc8, 0x32, 0x1e)).paint(
                &mut whole,
                Affine::IDENTITY,
                &theme,
                rect,
            );
            let mut start = Scene::new();
            StrokePreviewFace::new(character, 8.0, Color::from_rgb8(0xc8, 0x32, 0x1e))
                .revealed(0.0)
                .paint(&mut start, Affine::IDENTITY, &theme, rect);
            let drawn = |scene: &Scene| {
                scene
                    .encoding()
                    .draw_tags
                    .iter()
                    .filter(|tag| **tag == DrawTag::COLOR)
                    .count()
            };
            assert!(drawn(&whole) > 2, "{character:?} paints its stroke");
            assert!(
                drawn(&whole) >= drawn(&start),
                "{character:?} at its start draws no more than its whole"
            );
        }
    }

    #[test]
    fn the_preview_names_itself_and_asks_for_the_size_it_prefers() {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nodes = HashMap::new();
        let mut preview = StrokePreview::from_props("preview", props("marker"));
        let node = preview.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        let style = taffy.style(node).expect("the preview has a layout node");
        assert_eq!(
            style.min_size.width,
            Dimension::length(crate::num::f32_of_f64(PREFERRED_WIDTH))
        );
        assert_eq!(
            style.size.height,
            Dimension::length(crate::num::f32_of_f64(PREFERRED_HEIGHT))
        );
        let node = preview.accessibility().expect("a preview is announced");
        assert_eq!(
            node.name.as_deref(),
            Some(crate::i18n::widget_text("stroke-preview-name").as_str())
        );
    }
}
