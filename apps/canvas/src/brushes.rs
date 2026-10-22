//! Brush presets: how a stroke on a plain layer is laid down. Everything is
//! worked out from the stroke mark itself (its points and their pressure,
//! its size, colour, preset and dynamics), so every machine draws the same
//! stroke and editing the mark draws it again.
//!
//! Pen, marker, calligraphy and a hard eraser are outlines, crisp at any
//! zoom. Pencil, charcoal, airbrush and a soft eraser are dabs of paint
//! stamped along the stroke into pixels fixed to the artwork, where the
//! ground's tooth catches graphite and charcoal.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "float geometry, and pixel indices bounded by the raster they index"
)]

use std::collections::BTreeMap;
use ui::num::f32_of_f64;

use tilo_wet_media::StrokePoint as WetPoint;
use tilo_wet_media::brush::Sample;
use ui::vello::kurbo::{BezPath, Point, Rect, Shape as _};

use crate::model::{Mark, Shape, Style};

/// What lays a stroke down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Preset {
    /// The round ink brush: width follows pressure, ends taper.
    #[default]
    Round,
    /// A technical pen: crisp ink, pressure sets the width.
    Pen,
    /// Graphite: grainy, pressure sets how dark.
    Pencil,
    /// A felt marker: flat and see-through, overlaps darken.
    Marker,
    /// A soft spray that builds up where it passes again.
    Airbrush,
    /// Charcoal or chalk: dry, ragged, catches the ground's tooth.
    Charcoal,
    /// A broad nib held at an angle: thick across it, thin along it.
    Calligraphy,
    /// Takes paint off its layer.
    Eraser,
    /// Whole square artwork cells, with no pressure or softened edges.
    Pixel,
    /// Clears whole square artwork cells.
    PixelEraser,
}

/// The dynamics a stroke may set in its `params`, each `0..=1`.
pub const DYNAMICS: [&str; 5] = ["flow", "hardness", "spacing", "smoothing", "taper"];

/// How a preset handles: its preset values, overridden per stroke.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dynamics {
    /// How much paint each touch lays: the marker's ink, the airbrush's
    /// spray, the pencil's graphite.
    pub flow: f64,
    /// How crisp the edge is; a soft brush fades out over its outer part.
    pub hardness: f64,
    /// How far apart the dabs of a dabbed brush are, as a share of its size.
    pub spacing: f64,
    /// How much the hand's wobble is smoothed out of the path.
    pub smoothing: f64,
    /// How long the ends taper, from blunt (0) to long and pointed (1).
    pub taper: f64,
}

impl Preset {
    /// Every preset, in the picker's order.
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

    pub fn name(self) -> &'static str {
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

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.name() == name)
    }

    /// The preset a mark is drawn with.
    pub fn of(style: &Style) -> Self {
        style
            .brush
            .as_deref()
            .and_then(Self::parse)
            .unwrap_or_default()
    }

    pub fn names() -> String {
        Self::ALL.map(Self::name).join(", ")
    }

    /// The Fluent id of the preset's name.
    pub fn label(self) -> String {
        format!("canvas-brush-{}", self.name())
    }

    fn defaults(self) -> Dynamics {
        let (flow, hardness, spacing, smoothing, taper) = match self {
            Self::Round => (1.0, 1.0, 0.1, 0.0, 0.5),
            Self::Pen => (1.0, 1.0, 0.1, 0.15, 0.25),
            Self::Pencil => (0.45, 0.7, 0.12, 0.0, 0.15),
            Self::Marker => (0.7, 0.9, 0.1, 0.1, 0.0),
            Self::Airbrush => (0.08, 0.0, 0.06, 0.0, 0.0),
            Self::Charcoal => (0.6, 0.45, 0.1, 0.0, 0.2),
            Self::Calligraphy => (1.0, 1.0, 0.1, 0.2, 0.2),
            Self::Eraser | Self::Pixel | Self::PixelEraser => (1.0, 1.0, 0.1, 0.0, 0.0),
        };
        Dynamics {
            flow,
            hardness,
            spacing,
            smoothing,
            taper,
        }
    }

    pub fn pixel(self) -> bool {
        matches!(self, Self::Pixel | Self::PixelEraser)
    }

    pub fn erases(self) -> bool {
        matches!(self, Self::Eraser | Self::PixelEraser)
    }

    /// Whether the preset stamps dabs into pixels rather than filling an
    /// outline.
    pub fn dabbed(self, dynamics: &Dynamics) -> bool {
        match self {
            Self::Pencil | Self::Airbrush | Self::Charcoal => true,
            Self::Eraser => dynamics.hardness < 0.95,
            _ => false,
        }
    }

    /// How much the ground's tooth shows in what the preset lays.
    fn grain(self) -> f64 {
        match self {
            Self::Pencil => 0.85,
            Self::Charcoal => 1.0,
            _ => 0.0,
        }
    }
}

impl Dynamics {
    /// The dynamics `preset` handles with, as `params` override them.
    pub fn of(preset: Preset, params: &BTreeMap<String, f64>) -> Self {
        let mut dynamics = preset.defaults();
        for (name, field) in [
            ("flow", &mut dynamics.flow),
            ("hardness", &mut dynamics.hardness),
            ("spacing", &mut dynamics.spacing),
            ("smoothing", &mut dynamics.smoothing),
            ("taper", &mut dynamics.taper),
        ] {
            if let Some(value) = params.get(name) {
                *field = value.clamp(0.0, 1.0);
            }
        }
        dynamics
    }

    pub fn of_mark(mark: &Mark) -> Self {
        Self::of(Preset::of(&mark.style), &mark.style.params)
    }
}

/// The preset as a mark stores it: the round brush is no preset at all.
pub fn stored(name: &str) -> Option<String> {
    (!name.is_empty() && name != Preset::Round.name()).then(|| name.to_owned())
}

/// Why `brush` and `params` cannot draw a stroke on a plain layer, in
/// words an agent can act on.
pub fn problem(brush: Option<&str>, params: &BTreeMap<String, f64>, plain: bool) -> Option<String> {
    if let Some(brush) = brush
        && !brush.is_empty()
        && Preset::parse(brush).is_none()
    {
        return Some(format!(
            "brush {brush:?} is not a preset; use one of {}",
            Preset::names()
        ));
    }
    if !plain {
        if brush.and_then(Preset::parse).is_some_and(Preset::pixel) {
            return Some("pixel brushes need a plain layer".into());
        }
        return None;
    }
    for (name, value) in params {
        if !DYNAMICS.contains(&name.as_str()) {
            return Some(format!(
                "on a plain layer a stroke's params are {}; {name:?} is a medium's (give the layer a medium)",
                DYNAMICS.join(", ")
            ));
        }
        if !(0.0..=1.0).contains(value) {
            return Some(format!("{name} is 0..=1, got {value}"));
        }
    }
    None
}

fn point_of(point: &[f64]) -> (f64, f64, f64) {
    (
        point.first().copied().unwrap_or(0.0),
        point.get(1).copied().unwrap_or(0.0),
        point.get(2).copied().unwrap_or(1.0),
    )
}

/// `points` with the hand's wobble taken out. The stroke is first laid
/// along its smooth curve at an even spacing, so a stroke of a few points
/// keeps its shape; then the wobble is blurred out of it, the ends staying
/// put, while curves much rounder than the wobble keep their size.
/// `amount` 1 evens out wobble up to about ten units across; a half, about
/// five. The stroke's [`corners`] stay where they are, sharp: each run
/// between them is smoothed on its own. `size` is the brush's.
pub fn smoothed(points: &[Vec<f64>], amount: f64, size: f64) -> Vec<Vec<f64>> {
    let mut out: Vec<Vec<f64>> = Vec::new();
    for run in runs(points, amount, size) {
        // A run starts on the corner the one before ended on.
        let skip = usize::from(!out.is_empty());
        out.extend(smoothed_run(&run, amount).into_iter().skip(skip));
    }
    out
}

/// `points` split at the stroke's [`corners`] into runs, each starting on
/// the corner the one before ends on.
fn runs(points: &[Vec<f64>], amount: f64, size: f64) -> Vec<Vec<Vec<f64>>> {
    let mut runs = Vec::new();
    let mut from = 0;
    for corner in corners(points, amount, size) {
        runs.push(points.get(from..=corner).unwrap_or_default().to_vec());
        from = corner;
    }
    runs.push(points.get(from..).unwrap_or_default().to_vec());
    runs
}

/// The indices of the points where a stroke turns sharply: a corner the
/// hand or agent meant, kept sharp, not a bend smoothing rounds off. The
/// turn is judged across a reach that grows with the brush and with how
/// much wobble the smoothing takes out, so a shaky hand's zigzags are not
/// corners; and at least half of that turn must happen close to the point
/// (within a quarter of the reach, or two of the hand's steps), so a tight
/// curve, turning evenly all along, is not one either. Where several points
/// of one turn qualify, the sharpest is the corner.
pub fn corners(points: &[Vec<f64>], amount: f64, size: f64) -> Vec<usize> {
    if points.len() < 3 {
        return Vec::new();
    }
    let at: Vec<Point> = points
        .iter()
        .map(|point| {
            let (x, y, _) = point_of(point);
            Point::new(x, y)
        })
        .collect();
    let mut arc = Vec::with_capacity(at.len());
    let mut travelled = 0.0;
    let mut previous = at.first().copied().unwrap_or_default();
    for point in &at {
        travelled += (*point - previous).hypot();
        arc.push(travelled);
        previous = *point;
    }
    // Where the stroke is `distance` along it.
    let along = |distance: f64| {
        let distance = distance.clamp(0.0, travelled);
        let after = arc.partition_point(|reached| *reached < distance);
        match (
            after
                .checked_sub(1)
                .and_then(|before| at.get(before).zip(arc.get(before))),
            at.get(after).zip(arc.get(after)),
        ) {
            (Some((a, from)), Some((b, to))) if to - from > 1e-12 => {
                a.lerp(*b, (distance - from) / (to - from))
            }
            (_, Some((b, _))) => *b,
            _ => previous,
        }
    };
    // How far the stroke turns at `index` between `reach` before and after.
    let turn = |index: usize, reach: f64| {
        let (Some(point), Some(distance)) = (at.get(index), arc.get(index)) else {
            return 0.0;
        };
        let into = *point - along(distance - reach);
        let out = along(distance + reach) - *point;
        if into.hypot() < 1e-9 || out.hypot() < 1e-9 {
            return 0.0;
        }
        into.cross(out).atan2(into.dot(out)).abs()
    };
    let reach = (20.0 * amount.clamp(0.0, 1.0)).max(size).max(4.0);
    let mut steps: Vec<f64> = arc
        .windows(2)
        .filter_map(|pair| match pair {
            [a, b] if b - a > 1e-9 => Some(b - a),
            _ => None,
        })
        .collect();
    steps.sort_by(f64::total_cmp);
    let step = steps.get(steps.len() / 2).copied().unwrap_or(0.0);
    let close = (reach * 0.25).max(step * 2.0).min(reach);
    let corner = f64::from(tilo_wet_media::brush::CORNER);
    let mut corners: Vec<(usize, f64)> = Vec::new();
    let mut last_sharp: Option<usize> = None;
    for index in 1..at.len() - 1 {
        let whole = turn(index, reach);
        let sharp = turn(index, close);
        if whole <= corner || sharp <= whole * 0.5 {
            continue;
        }
        let same_turn = last_sharp.is_some_and(|last| {
            arc.get(index)
                .zip(arc.get(last))
                .is_some_and(|(here, there)| here - there < reach)
        });
        last_sharp = Some(index);
        match corners.last_mut() {
            Some(best) if same_turn => {
                if sharp > best.1 {
                    *best = (index, sharp);
                }
            }
            _ => corners.push((index, sharp)),
        }
    }
    corners.into_iter().map(|(index, _)| index).collect()
}

/// One run of a stroke between corners, smoothed: see [`smoothed`].
fn smoothed_run(points: &[Vec<f64>], amount: f64) -> Vec<Vec<f64>> {
    // Passes of a [1, 2, 1] kernel blur by about the square root of half
    // their number, in steps: 72 passes is six.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=72 before the cast"
    )]
    let passes = (amount.clamp(0.0, 1.0).powi(2) * 72.0).round() as usize;
    if passes == 0 || points.len() <= 2 {
        return points
            .iter()
            .map(|point| {
                let (x, y, pressure) = point_of(point);
                vec![x, y, pressure]
            })
            .collect();
    }
    let length: f64 = points
        .windows(2)
        .map(|pair| match pair {
            [a, b] => {
                let (a, b) = (point_of(a), point_of(b));
                (b.0 - a.0).hypot(b.1 - a.1)
            }
            _ => 0.0,
        })
        .sum();
    // Even steps of a unit or more, never more than the stroke may hold.
    let spacing = (length / 1000.0).max(1.0);
    let curve: Vec<WetPoint> = points
        .iter()
        .map(|point| {
            let (x, y, pressure) = point_of(point);
            WetPoint::new(f32_of_f64(x), f32_of_f64(y), f32_of_f64(pressure))
        })
        .collect();
    let mut points: Vec<(f64, f64, f64)> =
        tilo_wet_media::brush::smooth(&curve, f32_of_f64(spacing))
            .iter()
            .map(|sample| {
                (
                    f64::from(sample.x),
                    f64::from(sample.y),
                    f64::from(sample.pressure),
                )
            })
            .collect();
    if let (Some(last), Some(end)) = (points.last_mut(), curve.last()) {
        // The curve ends where the stroke does, not a step short of it.
        *last = (f64::from(end.x), f64::from(end.y), f64::from(end.pressure));
    }
    if points.len() <= 2 {
        return points
            .into_iter()
            .map(|(x, y, pressure)| vec![x, y, pressure])
            .collect();
    }
    // A blur alone pulls a tight curve in toward its chord as it evens the
    // wobble out. What the blur took is blurred too and given back
    // (`1 - (1 - B)^2`, Tukey's twicing): wobble a few units across still
    // goes, a curve many units round keeps its size.
    let mut detail: Vec<(f64, f64, f64)> = points.clone();
    for _ in 0..2 {
        let blurred = blur(&detail, passes);
        for (value, smooth) in detail.iter_mut().zip(&blurred) {
            value.0 -= smooth.0;
            value.1 -= smooth.1;
            value.2 -= smooth.2;
        }
    }
    for (point, residue) in points.iter_mut().zip(&detail) {
        point.0 -= residue.0;
        point.1 -= residue.1;
        point.2 -= residue.2;
    }
    points
        .into_iter()
        .map(|(x, y, pressure)| vec![x, y, pressure])
        .collect()
}

/// `values` blurred by `passes` of a `[1, 2, 1]` kernel, the ends held.
fn blur(values: &[(f64, f64, f64)], passes: usize) -> Vec<(f64, f64, f64)> {
    let mut values = values.to_vec();
    let mut before = values.clone();
    for _ in 0..passes {
        before.clone_from(&values);
        for (index, point) in values.iter_mut().enumerate().skip(1) {
            let (Some(previous), Some(next)) = (
                before.get(index.wrapping_sub(1)),
                before.get(index.saturating_add(1)),
            ) else {
                continue;
            };
            point.0 = 0.25 * previous.0 + 0.5 * point.0 + 0.25 * next.0;
            point.1 = 0.25 * previous.1 + 0.5 * point.1 + 0.25 * next.1;
            point.2 = 0.25 * previous.2 + 0.5 * point.2 + 0.25 * next.2;
        }
    }
    values
}

/// `points` with every point dropped that lies within `tolerance` units of
/// the line through those kept (Ramer-Douglas-Peucker); the ends stay.
pub fn simplified(points: &[Vec<f64>], tolerance: f64) -> Vec<Vec<f64>> {
    kept(points, tolerance)
        .into_iter()
        .filter_map(|index| points.get(index).cloned())
        .collect()
}

/// The indices of the points [`simplified`] keeps, first to last.
pub fn kept(points: &[Vec<f64>], tolerance: f64) -> Vec<usize> {
    if points.len() <= 2 {
        return (0..points.len()).collect();
    }
    let at = |index: usize| {
        let (x, y, _) = point_of(points.get(index).map_or(&[][..], Vec::as_slice));
        Point::new(x, y)
    };
    let last = points.len() - 1;
    let mut keep = vec![0, last];
    let mut spans = vec![(0, last)];
    while let Some((from, to)) = spans.pop() {
        let line = ui::vello::kurbo::Line::new(at(from), at(to));
        let mut worst = (0.0, from);
        for index in from + 1..to {
            use ui::vello::kurbo::ParamCurveNearest as _;
            let distance = line.nearest(at(index), 1e-9).distance_sq.sqrt();
            if distance > worst.0 {
                worst = (distance, index);
            }
        }
        if worst.0 > tolerance {
            keep.push(worst.1);
            spans.push((from, worst.1));
            spans.push((worst.1, to));
        }
    }
    keep.sort_unstable();
    keep
}

/// The stroke as evenly spaced samples `spacing` units apart, after its
/// smoothing, its corners sharp: each run between them is laid on its own
/// and ends exactly on its corner. `size` is the brush's.
pub fn samples(points: &[Vec<f64>], spacing: f64, smoothing: f64, size: f64) -> Vec<Sample> {
    let mut out: Vec<Sample> = Vec::new();
    for run in runs(points, smoothing, size) {
        let run: Vec<WetPoint> = smoothed_run(&run, smoothing)
            .iter()
            .map(|point| {
                let (x, y, pressure) = point_of(point);
                WetPoint::new(f32_of_f64(x), f32_of_f64(y), f32_of_f64(pressure))
            })
            .collect();
        let mut laid = tilo_wet_media::brush::smooth(&run, f32_of_f64(spacing));
        if let (Some(last), Some(end)) = (laid.last().copied(), run.last())
            && (end.x - last.x).hypot(end.y - last.y) > 1e-3
        {
            // The run ends on its corner, not up to a step short of it.
            laid.push(Sample {
                x: end.x,
                y: end.y,
                pressure: end.pressure,
                distance: last.distance + (end.x - last.x).hypot(end.y - last.y),
                ..last
            });
        }
        // A later run starts on the corner the one before ended on.
        let (skip, from) = match out.last() {
            Some(last) => (1, last.distance),
            None => (0, 0.0),
        };
        out.extend(laid.into_iter().skip(skip).map(|sample| Sample {
            distance: sample.distance + from,
            ..sample
        }));
    }
    out
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    if edge1 <= edge0 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How wide the stroke is `distance` along a `total` long stroke: the ends
/// thin over a length set by `taper`.
fn tapered(distance: f64, total: f64, size: f64, taper: f64) -> f64 {
    if taper <= 0.0 {
        return 1.0;
    }
    let length = (taper * size * 5.0).min(total * 0.45).max(1e-6);
    let start = smoothstep(0.0, length, distance);
    let end = smoothstep(0.0, length, total - distance);
    0.08 + 0.92 * start.min(end)
}

/// A stroke worked out for drawing: its samples and how wide it is at
/// each.
struct Laid {
    samples: Vec<Sample>,
    /// Half the width at each sample, in units.
    half: Vec<f64>,
}

fn sample_point(sample: &Sample) -> Point {
    Point::new(f64::from(sample.x), f64::from(sample.y))
}

/// How a preset turns pressure into width: the share of `size` at
/// pressure `p`.
fn width_of(preset: Preset, p: f64) -> f64 {
    match preset {
        Preset::Round | Preset::Pen => 0.3 + 0.7 * p,
        Preset::Pencil => 0.75 + 0.25 * p,
        Preset::Marker => 0.88 + 0.12 * p,
        Preset::Airbrush | Preset::Pixel | Preset::PixelEraser => 1.0,
        Preset::Charcoal => 0.6 + 0.4 * p,
        Preset::Calligraphy => 0.5 + 0.5 * p,
        Preset::Eraser => 0.45 + 0.55 * p,
    }
}

/// `points` without a last or first point that barely steps on from its
/// neighbour, far less than the step before (a hand's tiny hook as it
/// lifts, a repeated point), which would turn the brush at its very end;
/// the end keeps that point's pressure. `size` is the brush's.
pub fn steady_ends(points: &[Vec<f64>], size: f64) -> Vec<Vec<f64>> {
    let reach = (size * 0.2).clamp(0.5, 3.0);
    let step = |a: &[f64], b: &[f64]| {
        let (a, b) = (point_of(a), point_of(b));
        (b.0 - a.0).hypot(b.1 - a.1)
    };
    // An end step shorter than the brush's reach and than half the step
    // before it.
    let stub = |end: &[f64], next: &[f64], after: &[f64]| {
        let last = step(end, next);
        last < reach && last < 0.5 * step(next, after)
    };
    let mut points = points.to_vec();
    let n = points.len();
    if n > 3
        && let (Some(end), Some(before), Some(earlier)) =
            (points.get(n - 1), points.get(n - 2), points.get(n - 3))
        && stub(end, before, earlier)
    {
        let lift = point_of(end).2;
        points.pop();
        if let Some(end) = points.last_mut() {
            let (x, y, _) = point_of(end);
            *end = vec![x, y, lift];
        }
    }
    if points.len() > 3
        && let (Some(start), Some(after), Some(later)) =
            (points.first(), points.get(1), points.get(2))
        && stub(start, after, later)
    {
        let land = point_of(start).2;
        points.remove(0);
        if let Some(start) = points.first_mut() {
            let (x, y, _) = point_of(start);
            *start = vec![x, y, land];
        }
    }
    points
}

fn lay(points: &[Vec<f64>], size: f64, preset: Preset, dynamics: &Dynamics) -> Laid {
    let spacing = if preset.dabbed(dynamics) {
        (dynamics.spacing.max(0.02) * size).max(0.2)
    } else {
        (size * 0.08).clamp(0.2, 2.0)
    };
    let samples = samples(
        &steady_ends(points, size),
        spacing,
        dynamics.smoothing,
        size,
    );
    let total = samples.last().map_or(0.0, |last| f64::from(last.distance));
    let half = samples
        .iter()
        .map(|sample| {
            let p = f64::from(sample.pressure).clamp(0.0, 1.0);
            (size
                * 0.5
                * width_of(preset, p)
                * tapered(f64::from(sample.distance), total, size, dynamics.taper))
            .max(0.15)
        })
        .collect();
    Laid { samples, half }
}

/// The closed outline of a line of width `2 * half` along `samples`, with
/// round ends.
fn outline(samples: &[Sample], half: &[f64]) -> BezPath {
    let mut path = BezPath::new();
    let radius = |index: usize| half.get(index).copied().unwrap_or(0.5);
    let Some(first) = samples.first() else {
        return path;
    };
    if samples.len() == 1 {
        return ui::vello::kurbo::Shape::to_path(
            &ui::vello::kurbo::Circle::new(sample_point(first), radius(0)),
            0.1,
        );
    }
    let normals = normals(samples, half);
    let normal_at = |index: usize| normals.get(index).copied().unwrap_or((0.0, 1.0));
    let side = |sign: f64| {
        samples
            .iter()
            .enumerate()
            .map(|(index, sample)| {
                let (nx, ny) = normal_at(index);
                sample_point(sample) + (nx * sign * radius(index), ny * sign * radius(index))
            })
            .collect::<Vec<Point>>()
    };
    let left = side(1.0);
    let right = side(-1.0);
    let cap = |index: usize, r: f64, from_left: bool| -> Vec<Point> {
        let Some(sample) = samples.get(index) else {
            return Vec::new();
        };
        let (nx, ny) = normal_at(index);
        // Around the end from one side to the other, the long way round
        // the front of the stroke.
        let base = ny.atan2(nx) + if from_left { 0.0 } else { std::f64::consts::PI };
        (1..8)
            .map(|step| {
                let angle = base - std::f64::consts::PI * f64::from(step) / 8.0;
                sample_point(sample) + (r * angle.cos(), r * angle.sin())
            })
            .collect()
    };
    let mut ring: Vec<Point> = left;
    ring.extend(cap(samples.len() - 1, radius(samples.len() - 1), true));
    ring.extend(right.into_iter().rev());
    ring.extend(cap(0, radius(0), false));
    for (index, point) in ring.into_iter().enumerate() {
        if index == 0 {
            path.move_to(point);
        } else {
            path.line_to(point);
        }
    }
    path.close_path();
    path
}

/// The unit normal at each sample, its direction taken across as much of
/// the path as the line is wide there: a hand's last tiny hook or a near
/// repeated point at the lift would otherwise turn the line's end edge-on
/// and kink its outline.
fn normals(samples: &[Sample], half: &[f64]) -> Vec<(f64, f64)> {
    let last = samples.len().saturating_sub(1);
    let spacing = samples
        .get(1)
        .map_or(1.0, |second| f64::from(second.distance).max(1e-3));
    // A tapered end still turns as the full line does.
    let widest = half.iter().copied().fold(0.0, f64::max);
    samples
        .iter()
        .enumerate()
        .map(|(index, sample)| {
            let width = half.get(index).copied().unwrap_or(0.5).max(widest * 0.5);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 1..=24 before the cast"
            )]
            let reach = (width / spacing).ceil().clamp(1.0, 24.0) as usize;
            let (Some(from), Some(to)) = (
                samples.get(index.saturating_sub(reach)),
                samples.get((index + reach).min(last)),
            ) else {
                return (-f64::from(sample.ty), f64::from(sample.tx));
            };
            let along = sample_point(to) - sample_point(from);
            let length = along.hypot();
            if length > 1e-9 {
                (-along.y / length, along.x / length)
            } else {
                (-f64::from(sample.ty), f64::from(sample.tx))
            }
        })
        .collect()
}

/// A broad nib held at `angle` degrees dragged along the stroke: a quad
/// between each pair of samples, every one wound the same way so they
/// join, and a hairline where the nib runs along its edge.
fn nib(laid: &Laid, angle: f64, size: f64) -> BezPath {
    let (sin, cos) = angle.to_radians().sin_cos();
    let mut path = BezPath::new();
    let ends = |index: usize| {
        let sample = laid.samples.get(index)?;
        let half = laid.half.get(index).copied()?;
        let centre = sample_point(sample);
        Some((
            centre + (cos * half, sin * half),
            centre - (cos * half, sin * half),
        ))
    };
    for index in 1..laid.samples.len() {
        let (Some((a, b)), Some((c, d))) = (ends(index - 1), ends(index)) else {
            continue;
        };
        let mut quad = [a, c, d, b];
        let area: f64 = quad
            .iter()
            .zip(quad.iter().cycle().skip(1))
            .map(|(p, q)| p.x * q.y - q.x * p.y)
            .sum();
        if area < 0.0 {
            quad.reverse();
        }
        for (corner, at) in quad.into_iter().enumerate() {
            if corner == 0 {
                path.move_to(at);
            } else {
                path.line_to(at);
            }
        }
        path.close_path();
    }
    let hair: Vec<f64> = laid
        .half
        .iter()
        .map(|half| (half * 0.12).max(size * 0.03))
        .collect();
    let hairline = outline(&laid.samples, &hair);
    path.extend(wound_positive(hairline).elements().iter().copied());
    path
}

/// `path` wound counter-clockwise in y-down units (positive shoelace), so
/// it adds to, never cuts out of, what it is filled with.
fn wound_positive(path: BezPath) -> BezPath {
    let area = ui::vello::kurbo::Shape::area(&path);
    if area >= 0.0 {
        path
    } else {
        path.reverse_subpaths()
    }
}

/// The outline a stroke drawn with an outline preset fills, in the mark's
/// own units; for a dabbed preset, the region its dabs cover.
pub fn silhouette(pieces: &[Piece], style: &Style) -> BezPath {
    let preset = Preset::of(style);
    let dynamics = Dynamics::of(preset, &style.params);
    if preset.pixel() {
        return pixel_outline(pieces);
    }
    let mut path = BezPath::new();
    for (points, size) in pieces {
        let laid = lay(points, *size, preset, &dynamics);
        let piece = if preset == Preset::Calligraphy {
            nib(&laid, style.angle.unwrap_or(45.0), *size)
        } else {
            wound_positive(outline(&laid.samples, &laid.half))
        };
        path.extend(piece.elements().iter().copied());
    }
    path
}

/// Whole artwork cells along a stroke. Each square is part of one fill,
/// so revisiting a cell cannot accumulate opacity. The action path bounds
/// the travel of pixel strokes before this geometry is produced.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel travel is checked before rendering and brush widths are bounded"
)]
fn pixel_outline(pieces: &[Piece]) -> BezPath {
    let mut path = BezPath::new();
    for (points, size) in pieces {
        let width = size.round().max(1.0);
        let offset = (width / 2.0).floor();
        let mut cells = std::collections::BTreeSet::new();
        let mut previous = None;
        for point in points {
            let (x, y, _) = point_of(point);
            let cell = (x.floor() as i32, y.floor() as i32);
            let (from_x, from_y) = previous.unwrap_or(cell);
            let dx = f64::from(cell.0) - f64::from(from_x);
            let dy = f64::from(cell.1) - f64::from(from_y);
            let steps = dx.abs().max(dy.abs()).min(16384.0) as u32;
            for step in 0..=steps {
                let t = f64::from(step) / f64::from(steps.max(1));
                cells.insert((
                    (f64::from(from_x) + dx * t).round() as i32,
                    (f64::from(from_y) + dy * t).round() as i32,
                ));
            }
            previous = Some(cell);
        }
        for (x, y) in cells {
            let x = f64::from(x) - offset;
            let y = f64::from(y) - offset;
            path.extend(
                Rect::new(x, y, x + width, y + width)
                    .to_path(0.1)
                    .elements()
                    .iter()
                    .copied(),
            );
        }
    }
    path
}

/// How see-through a preset's outline is laid, before the mark's opacity:
/// a marker's ink is.
pub fn ink_alpha(style: &Style) -> f64 {
    let preset = Preset::of(style);
    match preset {
        Preset::Marker => Dynamics::of(preset, &style.params).flow,
        _ => 1.0,
    }
}

/// The blend a preset mixes with when the mark names none: a marker's
/// overlaps darken.
pub fn default_blend(style: &Style) -> Option<&'static str> {
    (Preset::of(style) == Preset::Marker).then_some("multiply")
}

/// A dabbed stroke as pixels: straight-alpha RGBA, `scale` pixels to a
/// unit, its top-left at `origin` in the mark's own units.
#[derive(Clone, Debug)]
pub struct Dabs {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub origin: Point,
    pub scale: f64,
}

/// The most pixels one dabbed stroke may take; a larger one is laid more
/// coarsely.
const MAX_PIXELS: f64 = 6.0e6;

/// What a dabbed stroke is laid with.
pub struct DabInput<'a> {
    /// The lines the stroke is laid as ([`pieces`]).
    pub pieces: &'a [Piece],
    pub style: &'a Style,
    /// Pixels per unit wanted.
    pub scale: f64,
    /// The paint's colour, `[r, g, b]`.
    pub rgb: [u8; 3],
    /// The ground's tooth at a point of the artwork, `0..=1`, mean a half.
    pub tooth: &'a dyn Fn(Point) -> f64,
    /// The mark's own units to the artwork's.
    pub to_artwork: ui::vello::kurbo::Affine,
}

/// Stamp a dabbed preset's dabs along the stroke.
pub fn dab(input: &DabInput<'_>) -> Option<Dabs> {
    let preset = Preset::of(input.style);
    let dynamics = Dynamics::of(preset, &input.style.params);
    let laid: Vec<Laid> = input
        .pieces
        .iter()
        .map(|(points, size)| lay(points, *size, preset, &dynamics))
        .collect();
    let mut bounds: Option<Rect> = None;
    for piece in &laid {
        let reach = piece.half.iter().copied().fold(0.0, f64::max) + 1.0;
        for sample in &piece.samples {
            let around = Rect::from_center_size(sample_point(sample), (reach * 2.0, reach * 2.0));
            bounds = Some(bounds.map_or(around, |bounds| bounds.union(around)));
        }
    }
    let bounds = bounds?;
    let scale = input
        .scale
        .max(0.05)
        .min((MAX_PIXELS / bounds.area().max(1e-6)).sqrt());
    let mut grid = Grid::new(bounds, scale);
    let tip = Tip {
        soft: 1.0 - dynamics.hardness.clamp(0.0, 1.0),
        ragged: if preset == Preset::Charcoal {
            0.35
        } else {
            0.0
        },
        spray: preset == Preset::Airbrush,
    };
    for piece in &laid {
        for (index, sample) in piece.samples.iter().enumerate() {
            let p = f64::from(sample.pressure).clamp(0.0, 1.0);
            let flow = match preset {
                Preset::Pencil => dynamics.flow * p.powf(1.4),
                Preset::Charcoal => dynamics.flow * (0.35 + 0.65 * p),
                Preset::Airbrush => dynamics.flow * (0.2 + 0.8 * p),
                _ => dynamics.flow,
            };
            let half = piece.half.get(index).copied().unwrap_or(0.5);
            grid.stamp(sample, half, flow, &tip);
        }
    }
    let pixels = grid.coloured(preset, input);
    Some(Dabs {
        pixels,
        width: u32::try_from(grid.width).ok()?,
        height: u32::try_from(grid.height).ok()?,
        origin: grid.origin,
        scale: grid.scale,
    })
}

/// How one dab touches the paper.
struct Tip {
    /// The share of the radius the edge fades over.
    soft: f64,
    /// How unevenly the edge breaks up, as dry charcoal does.
    ragged: f64,
    /// A spray: dense in the middle, thinning all the way out.
    spray: bool,
}

/// The paint a dabbed stroke lays, `0..=1` per pixel.
struct Grid {
    cover: Vec<f32>,
    width: usize,
    height: usize,
    origin: Point,
    scale: f64,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "pixel extents are bounded by MAX_PIXELS"
)]
impl Grid {
    fn new(bounds: Rect, scale: f64) -> Self {
        let width = (bounds.width() * scale).ceil().max(1.0) as usize;
        let height = (bounds.height() * scale).ceil().max(1.0) as usize;
        Self {
            cover: vec![0.0; width * height],
            width,
            height,
            origin: bounds.origin(),
            scale,
        }
    }

    /// One dab `half` units wide around `sample`, laying `flow` of paint
    /// over what is there.
    fn stamp(&mut self, sample: &Sample, half: f64, flow: f64, tip: &Tip) {
        let radius = half * self.scale;
        // Never softer than a pixel, so a fine pencil stays smooth.
        let edge = (tip.soft * radius).max(1.0);
        let centre = (sample_point(sample) - self.origin) * self.scale;
        let x0 = (centre.x - radius - 1.0).floor().max(0.0) as usize;
        let y0 = (centre.y - radius - 1.0).floor().max(0.0) as usize;
        let x1 = ((centre.x + radius + 1.0).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((centre.y + radius + 1.0).ceil().max(0.0) as usize).min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f64 + 0.5 - centre.x;
                let dy = y as f64 + 0.5 - centre.y;
                let distance = dx.hypot(dy);
                let touch = if tip.spray {
                    let t = (distance / radius.max(1e-6)).min(1.0);
                    (1.0 - t * t).powi(2)
                } else {
                    let mut reach = radius;
                    if tip.ragged > 0.0 {
                        let angle = dy.atan2(dx);
                        let along = f64::from(sample.distance) * 0.7;
                        reach *= 1.0
                            - tip.ragged * noise(along + angle.cos() * 2.0, angle.sin() * 2.0, 11);
                    }
                    1.0 - smoothstep(reach - edge, reach, distance)
                };
                if touch > 0.0
                    && let Some(value) = self.cover.get_mut(y * self.width + x)
                {
                    let laid = (flow * touch) as f32;
                    *value += laid * (1.0 - *value);
                }
            }
        }
    }

    /// The paint as straight-alpha RGBA, the ground's tooth catching it
    /// where the preset has grain.
    fn coloured(&self, preset: Preset, input: &DabInput<'_>) -> Vec<u8> {
        let grain = preset.grain();
        // How far pressing fills the tooth's valleys: graphite never quite
        // does, dry charcoal less still.
        let fill = if preset == Preset::Charcoal {
            0.55
        } else {
            0.6
        };
        let mut pixels = vec![0_u8; self.cover.len() * 4];
        for (index, value) in self.cover.iter().enumerate() {
            if *value <= 0.0 {
                continue;
            }
            let laid = f64::from(*value);
            let alpha = if grain > 0.0 {
                let local = self.origin
                    + ui::vello::kurbo::Vec2::new(
                        ((index % self.width) as f64 + 0.5) / self.scale,
                        ((index / self.width) as f64 + 0.5) / self.scale,
                    );
                let at = input.to_artwork * local;
                // Light pressure touches only the tooth's peaks; pressing
                // harder fills the valleys too.
                let tooth = (input.tooth)(at) * 0.5
                    + noise(at.x / 1.1, at.y / 1.1, 7) * 0.3
                    + noise(at.x / 0.4, at.y / 0.4, 13) * 0.2;
                let threshold = 1.0 - laid * fill;
                let caught = smoothstep(threshold - 0.2, threshold + 0.2, tooth);
                laid * (1.0 - grain + grain * caught)
            } else {
                laid
            };
            let byte = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
            if let Some(pixel) = pixels.get_mut(index * 4..index * 4 + 4) {
                pixel.copy_from_slice(&[input.rgb[0], input.rgb[1], input.rgb[2], byte]);
            }
        }
        pixels
    }
}

/// A `SplitMix64` hash of `value`.
fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "lattice coordinates are hashed; wrapping far from the origin is harmless"
)]
fn lattice(x: f64, y: f64, seed: u64) -> f64 {
    let xi = (x as i64).cast_unsigned();
    let yi = (y as i64).cast_unsigned();
    (mix(seed ^ mix(xi ^ mix(yi))) >> 11) as f64 / (1_u64 << 53) as f64
}

/// Smooth lattice noise in `0..=1`, fixed to the artwork.
fn noise(x: f64, y: f64, seed: u64) -> f64 {
    let (x0, y0) = (x.floor(), y.floor());
    let tx = smoothstep(0.0, 1.0, x - x0);
    let ty = smoothstep(0.0, 1.0, y - y0);
    let corner = |dx: f64, dy: f64| lattice(x0 + dx, y0 + dy, seed);
    let top = corner(0.0, 0.0) + (corner(1.0, 0.0) - corner(0.0, 0.0)) * tx;
    let bottom = corner(0.0, 1.0) + (corner(1.0, 1.0) - corner(0.0, 1.0)) * tx;
    top + (bottom - top) * ty
}

/// One line a stroke is laid as: its points and its size.
pub type Piece = (Vec<Vec<f64>>, f64);

/// The lines a stroke mark is laid as: itself, or every twig its `forks`
/// grow or dab its `dabs` scatter into, grown from the mark's id as a
/// painted layer grows them. Every preset lays each the way it lays a
/// stroke. A calligraphy nib's `angle` is the nib's, so its dabs follow
/// the path.
pub fn pieces(mark: &Mark) -> Vec<Piece> {
    let Some((points, size)) = stroke_of(mark) else {
        return Vec::new();
    };
    let angle = mark
        .style
        .angle
        .filter(|_| Preset::of(&mark.style) != Preset::Calligraphy);
    let Some(form) = mark.shape.form(angle) else {
        return vec![(points.to_vec(), size)];
    };
    let stroke = tilo_wet_media::engine::Stroke {
        points: points
            .iter()
            .map(|point| {
                let (x, y, pressure) = point_of(point);
                WetPoint::new(f32_of_f64(x), f32_of_f64(y), f32_of_f64(pressure))
            })
            .collect(),
        size: f32_of_f64(size),
        paint: tilo_wet_media::Paint {
            colour: tilo_wet_media::Rgb::WHITE,
            alpha: 1.0,
            granulation: 0.0,
            strength: 1.0,
        },
        opacity: 1.0,
        wet: false,
        params: BTreeMap::new(),
        form: Some(form),
        clip: Vec::new(),
    };
    stroke
        .pieces(tilo_wet_media::painter::seed_of(&mark.id))
        .into_iter()
        .map(|(piece, _)| {
            let points = piece
                .points
                .iter()
                .map(|point| {
                    vec![
                        f64::from(point.x),
                        f64::from(point.y),
                        f64::from(point.pressure),
                    ]
                })
                .collect();
            (points, f64::from(piece.size))
        })
        .collect()
}

/// The path a stroke's brush follows, after its smoothing, in the stroke's
/// own units: the line every preset lays its width along, and what the
/// point editor shows between the points.
pub fn centreline(points: &[Vec<f64>], size: f64, smoothing: f64) -> BezPath {
    let mut path = BezPath::new();
    let points = steady_ends(points, size);
    for (index, sample) in samples(&points, 0.5, smoothing, size).iter().enumerate() {
        if index == 0 {
            path.move_to(sample_point(sample));
        } else {
            path.line_to(sample_point(sample));
        }
    }
    path
}

/// The stroke `points` of a stroke mark, with its size.
pub fn stroke_of(mark: &Mark) -> Option<(&[Vec<f64>], f64)> {
    match &mark.shape {
        Shape::Stroke { points, size, .. } => Some((points.as_slice(), *size)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ui::vello::kurbo::Shape as _;

    use super::*;

    fn line(pressure: f64) -> Vec<Vec<f64>> {
        (0..=20)
            .map(|step| vec![f64::from(step) * 10.0, 50.0, pressure])
            .collect()
    }

    fn style(brush: &str) -> Style {
        Style {
            brush: stored(brush),
            ..Style::default()
        }
    }

    fn darkness(dabs: &Dabs) -> f64 {
        dabs.pixels
            .chunks_exact(4)
            .map(|pixel| f64::from(pixel[3]))
            .sum::<f64>()
    }

    fn flat(_: Point) -> f64 {
        0.5
    }

    fn laid(brush: &str, pressure: f64) -> Dabs {
        let points = line(pressure);
        let style = style(brush);
        dab(&DabInput {
            pieces: &[(points, 6.0)],
            style: &style,
            scale: 2.0,
            rgb: [20, 20, 20],
            tooth: &flat,
            to_artwork: ui::vello::kurbo::Affine::IDENTITY,
        })
        .expect("laid")
    }

    #[test]
    fn presets_round_trip_by_name_and_round_is_stored_as_none() {
        for preset in Preset::ALL {
            assert_eq!(Preset::parse(preset.name()), Some(preset));
        }
        assert_eq!(stored("round"), None);
        assert_eq!(stored(""), None);
        assert_eq!(stored("pencil").as_deref(), Some("pencil"));
        assert!(problem(Some("crayon"), &BTreeMap::new(), true).is_some());
        let medium = BTreeMap::from([("load".to_owned(), 0.3)]);
        assert!(problem(Some("pen"), &medium, true).is_some());
        assert!(problem(Some("pen"), &medium, false).is_none());
    }

    #[test]
    fn pencil_pressure_sets_darkness_not_width() {
        let light = laid("pencil", 0.2);
        let firm = laid("pencil", 1.0);
        assert!(darkness(&firm) > darkness(&light) * 2.5);
        assert!(light.height.abs_diff(firm.height) <= 2);
    }

    #[test]
    fn pen_pressure_sets_width() {
        let thin = silhouette(&[(line(0.1), 8.0)], &style("pen"));
        let wide = silhouette(&[(line(1.0), 8.0)], &style("pen"));
        assert!(wide.bounding_box().height() > thin.bounding_box().height() * 1.8);
    }

    #[test]
    fn the_same_stroke_lays_the_same_pixels() {
        assert_eq!(laid("charcoal", 0.7).pixels, laid("charcoal", 0.7).pixels);
    }

    #[test]
    fn a_calligraphy_nib_is_thick_across_its_angle_and_thin_along_it() {
        let across = Style {
            angle: Some(90.0),
            ..style("calligraphy")
        };
        let along = Style {
            angle: Some(0.0),
            ..style("calligraphy")
        };
        let thick = silhouette(&[(line(1.0), 10.0)], &across)
            .bounding_box()
            .height();
        let thin = silhouette(&[(line(1.0), 10.0)], &along)
            .bounding_box()
            .height();
        assert!(thick > thin * 3.0, "{thick} vs {thin}");
    }

    #[test]
    fn smoothing_and_simplifying_keep_the_ends() {
        let wobbly: Vec<Vec<f64>> = (0..40)
            .map(|step| {
                let y = if step % 2 == 0 { 3.0 } else { -3.0 };
                vec![f64::from(step) * 5.0, y, 1.0]
            })
            .collect();
        let smooth = smoothed(&wobbly, 1.0, 4.0);
        assert_eq!(smooth.first(), Some(&vec![0.0, 3.0, 1.0]));
        assert!(
            smooth
                .iter()
                .filter(|point| (30.0..170.0).contains(&point[0]))
                .all(|point| point[1].abs() < 1.2)
        );
        let fewer = simplified(&smooth, 1.5);
        assert!(fewer.len() < 12, "{}", fewer.len());
        assert_eq!(fewer.last(), smooth.last());
    }
    /// The points a path runs through.
    fn path_points(path: &BezPath) -> Vec<Point> {
        path.elements()
            .iter()
            .filter_map(|element| match element {
                ui::vello::kurbo::PathEl::MoveTo(at) | ui::vello::kurbo::PathEl::LineTo(at) => {
                    Some(*at)
                }
                _ => None,
            })
            .collect()
    }

    fn nearest(path: &[Point], to: (f64, f64)) -> f64 {
        path.iter()
            .map(|at| (at.x - to.0).hypot(at.y - to.1))
            .fold(f64::MAX, f64::min)
    }

    /// A little deterministic wobble in `-amount..=amount`.
    fn wobble(step: u32, amount: f64) -> f64 {
        f64::from(step.wrapping_mul(7919) % 13) / 6.0 * amount - amount
    }

    #[test]
    fn a_square_or_triangle_in_one_stroke_keeps_its_corners() {
        let pen = Dynamics::of(Preset::Pen, &BTreeMap::new());
        let square = [
            (0.0, 0.0),
            (300.0, 0.0),
            (300.0, 300.0),
            (0.0, 300.0),
            (0.0, 0.0),
        ];
        let triangle = [(0.0, 280.0), (150.0, 0.0), (300.0, 280.0), (0.0, 280.0)];
        for shape in [&square[..], &triangle[..]] {
            let points: Vec<Vec<f64>> = shape.iter().map(|(x, y)| vec![*x, *y, 1.0]).collect();
            assert_eq!(corners(&points, pen.smoothing, 8.0).len(), shape.len() - 2);
            let line = path_points(&centreline(&points, 8.0, pen.smoothing));
            for corner in &shape[1..shape.len() - 1] {
                let near = nearest(&line, *corner);
                assert!(near < 0.3, "corner {corner:?} missed by {near}");
            }
            // The sides run straight between the corners: none bulges.
            for at in &line {
                let off = shape
                    .windows(2)
                    .map(|pair| {
                        use ui::vello::kurbo::ParamCurveNearest as _;
                        let side = ui::vello::kurbo::Line::new(pair[0], pair[1]);
                        side.nearest(*at, 1e-9).distance_sq.sqrt()
                    })
                    .fold(f64::MAX, f64::min);
                assert!(off < 0.3, "{at:?} is {off} off the shape");
            }
        }
    }

    #[test]
    fn a_shaky_hands_square_keeps_its_corners_and_loses_its_shake() {
        // A square 250 across drawn by hand: a point every 3 units, each
        // up to a unit off the line.
        let mut points = Vec::new();
        let corners_at = [
            (0.0, 0.0),
            (250.0, 0.0),
            (250.0, 250.0),
            (0.0, 250.0),
            (0.0, 0.0),
        ];
        let mut step = 0;
        for pair in corners_at.windows(2) {
            let ((ax, ay), (bx, by)) = (pair[0], pair[1]);
            for along in 0..84 {
                let t = f64::from(along) / 84.0;
                step += 1;
                points.push(vec![
                    ax + (bx - ax) * t + wobble(step, 1.0),
                    ay + (by - ay) * t + wobble(step * 3, 1.0),
                    1.0,
                ]);
            }
        }
        points.push(vec![0.0, 0.0, 1.0]);
        let found = corners(&points, 0.6, 8.0);
        assert_eq!(found.len(), 3);
        let line = path_points(&centreline(&points, 8.0, 0.6));
        for (corner, index) in corners_at[1..4].iter().zip(&found) {
            // The line turns exactly on the point the hand turned at, which
            // is within its shake of the true corner.
            let turned = &points[*index];
            assert!(nearest(&line, (turned[0], turned[1])) < 1e-3);
            let near = nearest(&line, *corner);
            assert!(near < 2.0, "corner {corner:?} missed by {near}");
        }
        for at in line
            .iter()
            .filter(|at| at.y < 50.0 && (60.0..190.0).contains(&at.x))
        {
            assert!(at.y.abs() < 0.6, "{at:?} still shakes");
        }
    }

    #[test]
    fn a_gentle_noisy_curve_has_no_corners_and_is_smoothed() {
        // A third of a circle 150 round, a point every 2 units, each up to a
        // unit off it.
        let points: Vec<Vec<f64>> = (0..=157_u32)
            .map(|step| {
                let angle = f64::from(step) * 2.0 / 150.0;
                let radius = 150.0 + wobble(step, 1.0);
                vec![radius * angle.cos(), radius * angle.sin(), 1.0]
            })
            .collect();
        assert!(corners(&points, 0.6, 8.0).is_empty());
        let smooth = smoothed(&points, 0.6, 8.0);
        let middle = &smooth[smooth.len() / 5..smooth.len() * 4 / 5];
        for point in middle {
            let radius = point[0].hypot(point[1]);
            assert!((radius - 150.0).abs() < 0.5, "radius {radius}");
        }
    }

    #[test]
    fn smoothing_keeps_a_tight_curve_its_size() {
        // Most of a circle 12 units round, a point every 15 degrees.
        let arc: Vec<Vec<f64>> = (0..=20)
            .map(|step| {
                let angle = f64::from(step) * 15_f64.to_radians();
                vec![100.0 + 12.0 * angle.cos(), 100.0 + 12.0 * angle.sin(), 1.0]
            })
            .collect();
        let smooth = smoothed(&arc, 1.0, 4.0);
        let middle = &smooth[smooth.len() / 4..smooth.len() * 3 / 4];
        for point in middle {
            let radius = (point[0] - 100.0).hypot(point[1] - 100.0);
            assert!((radius - 12.0).abs() < 0.36, "radius {radius}");
        }
    }

    #[test]
    fn a_hook_at_the_lift_does_not_turn_the_lines_end() {
        // A pen line whose last point steps back a little across it, as a
        // hand's lift often does.
        let mut points = line(0.8);
        points.push(vec![199.7, 50.6, 0.35]);
        let dynamics = Dynamics::of(Preset::Pen, &BTreeMap::new());
        let laid = lay(&points, 8.0, Preset::Pen, &dynamics);
        for (nx, ny) in normals(&laid.samples, &laid.half) {
            assert!(ny.abs() > 0.94 && nx.abs() < 0.35, "normal ({nx}, {ny})");
        }
        // Its outline stays as tall as the line, no notch or spike at the end.
        let outline = silhouette(&[(points, 8.0)], &style("pen"));
        let bounds = outline.bounding_box();
        assert!(
            bounds.max_x() < 205.0 && bounds.height() < 9.0,
            "{bounds:?}"
        );
    }

    #[test]
    fn the_editor_shows_the_curve_the_brush_follows() {
        // Three points: the line bows through the middle one, it does not
        // run straight to it.
        let points = vec![vec![0.0, 0.0], vec![50.0, 30.0], vec![100.0, 0.0]];
        let curve = centreline(&points, 2.0, 0.0);
        let at_quarter = curve
            .elements()
            .iter()
            .filter_map(ui::vello::kurbo::PathEl::end_point)
            .min_by(|a, b| (a.x - 25.0).abs().total_cmp(&(b.x - 25.0).abs()))
            .unwrap();
        // A straight segment would be at y 15 there; the spline is higher.
        assert!(at_quarter.y > 18.0, "{at_quarter:?}");
        let pen = silhouette(&[(points, 2.0)], &style("pen")).bounding_box();
        assert!((pen.max_y() - curve.bounding_box().max_y()).abs() < 1.6);
    }

    #[test]
    fn every_preset_grows_forks_and_dabs() {
        let mark = |brush: &str, forks: Option<u32>, dabs: Option<u32>| Mark {
            id: "tree".into(),
            layer: "l".into(),
            order: "a".into(),
            name: String::new(),
            shape: Shape::Stroke {
                points: vec![vec![100.0, 200.0], vec![100.0, 100.0]],
                size: 8.0,
                forks,
                spread: None,
                dabs,
                scatter: Some(30.0),
                dab_length: None,
                hatch: None,
            },
            style: style(brush),
            transform: None,
            group: String::new(),
            clip: String::new(),
            area: String::new(),
        };
        for preset in ["pen", "marker", "calligraphy"] {
            let plain = silhouette(&pieces(&mark(preset, None, None)), &style(preset));
            let tree = silhouette(&pieces(&mark(preset, Some(3), None)), &style(preset));
            let leaves = silhouette(&pieces(&mark(preset, None, Some(40))), &style(preset));
            assert!(
                tree.bounding_box().width() > plain.bounding_box().width() * 2.0,
                "{preset} forks"
            );
            assert!(
                leaves.bounding_box().width() > plain.bounding_box().width() * 2.0,
                "{preset} dabs"
            );
        }
        for preset in ["pencil", "airbrush", "charcoal"] {
            let laid = |forks| {
                let mark = mark(preset, forks, None);
                dab(&DabInput {
                    pieces: &pieces(&mark),
                    style: &mark.style,
                    scale: 1.0,
                    rgb: [20, 20, 20],
                    tooth: &flat,
                    to_artwork: ui::vello::kurbo::Affine::IDENTITY,
                })
                .expect("laid")
            };
            assert!(laid(Some(3)).width > laid(None).width * 2, "{preset} forks");
        }
    }
}
