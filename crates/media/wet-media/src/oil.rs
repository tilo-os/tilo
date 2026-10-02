//! Opaque oil paint.
//!
//! Each cell holds the colour of the paint on top, how completely it covers
//! the ground, how thick it is and how wet. A brush is a row of bristles,
//! each with its own load that runs out along the stroke, so a long stroke
//! breaks into streaks and a dry brush catches only the high points of the
//! weave. Laid into wet paint, a brush picks colour up and drags it, so
//! strokes blend wet-in-wet; laid over dry paint, it covers. Thickness is
//! lit from the upper left, so impasto shows its ridges.

use std::sync::Arc;

use rayon::prelude::*;

use crate::brush::{Sample, length, smooth, taper};
use crate::colour::{Rgb, encode8, from_linear};
use crate::engine::{Blend, Canvas, Fill, LayerSim, Mark, MediumEngine, ParamSpec, Settings};
use crate::grid::{Area, at, cell, float, key, read, smoothstep};
use crate::ground::{fbm, hash01, mix};
use crate::mask::{Coverage, fill};

/// The oil engine.
pub struct Oil {
    params: Arc<[ParamSpec]>,
}

impl Oil {
    pub fn new() -> Self {
        Self {
            params: vec![
                ParamSpec::new(
                    "load",
                    "Paint load",
                    "How much paint the brush carries; low loads drag dry and broken over the weave.",
                    (0.05, 1.5),
                    0.8,
                ),
                ParamSpec::new(
                    "thickness",
                    "Thickness",
                    "How much body the paint has; thick paint stands in ridges (impasto).",
                    (0.0, 1.0),
                    0.45,
                ),
                ParamSpec::new(
                    "bristles",
                    "Bristles",
                    "How many bristles the brush has; few bristles leave visible streaks.",
                    (3.0, 64.0),
                    22.0,
                ),
                ParamSpec::new(
                    "streak",
                    "Streakiness",
                    "How unevenly the bristles carry paint.",
                    (0.0, 1.0),
                    0.45,
                ),
                ParamSpec::new(
                    "blend",
                    "Wet blending",
                    "How much wet paint already on the layer the brush picks up and drags.",
                    (0.0, 1.0),
                    0.4,
                ),
                ParamSpec::new(
                    "relief",
                    "Relief",
                    "How strongly the light shows the paint's ridges.",
                    (0.0, 1.0),
                    0.6,
                ),
            ]
            .into(),
        }
    }
}

impl Default for Oil {
    fn default() -> Self {
        Self::new()
    }
}

impl MediumEngine for Oil {
    fn id(&self) -> &'static str {
        "oil"
    }

    fn label(&self) -> &'static str {
        "Oil"
    }

    fn description(&self) -> &'static str {
        "Opaque paint with body: bristle strokes that streak and run dry, blend into wet paint, cover dry paint and stand in lit ridges."
    }

    fn parameters(&self) -> Arc<[ParamSpec]> {
        Arc::clone(&self.params)
    }

    fn blend(&self) -> Blend {
        Blend::Normal
    }

    fn new_layer(&self, canvas: &Canvas, settings: Settings) -> Box<dyn LayerSim> {
        let cells = canvas.width.saturating_mul(canvas.height);
        Box::new(OilLayer {
            canvas: canvas.clone(),
            relief: settings.get("relief"),
            settings,
            colour: std::array::from_fn(|_| vec![0.0; cells]),
            cover: vec![0.0; cells],
            thickness: vec![0.0; cells],
            wet: vec![0.0; cells],
            wet_area: Area::EMPTY,
        })
    }
}

#[derive(Clone)]
struct OilLayer {
    canvas: Canvas,
    settings: Settings,
    relief: f32,
    /// Linear colour of the paint on top, per channel.
    colour: [Vec<f32>; 3],
    /// How completely the paint hides the ground, `0..=1`.
    cover: Vec<f32>,
    /// Paint thickness, in arbitrary units around `0..=1`.
    thickness: Vec<f32>,
    /// How wet the paint on top is, `0..=1`.
    wet: Vec<f32>,
    wet_area: Area,
}

/// One bristle of a brush as it travels a stroke.
struct Bristle {
    /// Offset across the brush, `-1..=1`.
    offset: f32,
    /// Paint left on the bristle.
    load: f32,
    /// How fast it runs out.
    spend: f32,
    /// The bristle's paint colour, drifting as it picks up wet paint.
    colour: Rgb,
    /// How high a ridge it leaves, `0..=1`.
    ridge: f32,
}

/// What one bristle dab laid and picked up.
struct Dab {
    spent: f32,
    picked: Rgb,
    picked_weight: f32,
    area: Area,
}

/// Everything one brush pass needs besides the layer.
struct Pass<'a> {
    samples: &'a [Sample],
    size: f32,
    colour: Rgb,
    opacity: f32,
    load: f32,
    seed: u64,
    clip: Option<&'a Coverage>,
}

impl OilLayer {
    fn width(&self) -> usize {
        self.canvas.width
    }

    fn dry(&mut self) {
        if self.wet_area.is_empty() {
            return;
        }
        let width = self.width();
        let area = self.wet_area;
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                if let Some(value) = self.wet.get_mut(at(width, x, y)) {
                    *value = 0.0;
                }
            }
        }
        self.wet_area = Area::EMPTY;
    }

    /// The bristles of a fresh brushload.
    fn bristles(settings: &Settings, pass: &Pass<'_>) -> Vec<Bristle> {
        // A thin brush has fewer bristles than its preset says: a line two
        // cells wide cannot hold twenty streaks.
        let count = cell(settings.get("bristles").min(pass.size * 0.9), 65).max(3);
        let streak = settings.get("streak");
        (0..count)
            .map(|index| {
                let along = (float(index) + 0.5) / float(count);
                let jitter = hash01(pass.seed, 11, key(index)) - 0.5;
                let offset = (along * 2.0 - 1.0 + jitter / float(count)).clamp(-1.0, 1.0);
                let uneven = hash01(pass.seed, 13, key(index));
                // How far, in cells, a full bristle travels before it is dry.
                let reach = pass.size * (10.0 + 30.0 * (1.0 - streak * uneven)) + 80.0;
                let shade = 1.0 + (hash01(pass.seed, 19, key(index)) - 0.5) * 0.1 * (0.5 + streak);
                Bristle {
                    offset,
                    load: pass.load * (1.0 - streak * 0.6 * uneven),
                    spend: 1.0 / (reach * (0.6 + 0.8 * hash01(pass.seed, 17, key(index)))),
                    colour: pass.colour.scale(shade).clamp(),
                    ridge: hash01(pass.seed, 23, key(index)),
                }
            })
            .collect()
    }

    /// Drag a brush along `pass.samples`, depositing and picking up paint.
    fn brush(&mut self, settings: &Settings, pass: &Pass<'_>) {
        let total = length(pass.samples);
        if pass.samples.is_empty() || pass.size <= 0.0 {
            return;
        }
        let mut bristles = Self::bristles(settings, pass);
        let blend = settings.get("blend");
        let tip = (pass.size / float(bristles.len().max(1)) * 1.3).max(0.9);
        // The paint a full deposit under a bristle's cone-shaped tip lays.
        let footprint = (std::f32::consts::PI * tip * tip / 3.0).max(0.5);
        let body = settings.get("thickness");
        let mut touched = Area::EMPTY;
        let mut previous: Option<(f32, f32)> = None;
        for sample in pass.samples {
            let travelled = previous.map_or(0.0, |(x, y)| (sample.x - x).hypot(sample.y - y));
            previous = Some((sample.x, sample.y));
            let half = pass.size
                * 0.5
                * (0.35 + 0.65 * sample.pressure)
                * taper(sample.distance, total, pass.size);
            for bristle in &mut bristles {
                if bristle.load <= 0.002 {
                    continue;
                }
                let centre = (
                    sample.x - sample.ty * bristle.offset * half,
                    sample.y + sample.tx * bristle.offset * half,
                );
                let dab = self.dab(bristle, centre, tip, body, pass);
                // Wet paint beneath sticks to the bristle and travels with
                // it, by how far the bristle dragged through it.
                if dab.picked_weight > 0.0 {
                    let share = (dab.picked_weight / footprint).min(1.0);
                    bristle.colour = bristle
                        .colour
                        .lerp(dab.picked, (blend * share * travelled * 0.04).min(0.5));
                }
                // A bristle spends paint by the distance it drags, at the
                // rate it actually lays it: over dry ground it lays less.
                let laying = (dab.spent / footprint).min(1.0);
                bristle.load = (bristle.load - travelled * laying * bristle.spend).max(0.0);
                touched = touched.union(dab.area);
            }
        }
        self.wet_area = self.wet_area.union(touched);
    }

    /// One bristle's tip pressed at `centre`.
    fn dab(
        &mut self,
        bristle: &Bristle,
        centre: (f32, f32),
        tip: f32,
        body: f32,
        pass: &Pass<'_>,
    ) -> Dab {
        let (width, height) = (self.width(), self.canvas.height);
        let reach = cell(tip.ceil() + 1.0, 64);
        let area = Area {
            x0: cell(centre.0 - tip, width).saturating_sub(1),
            y0: cell(centre.1 - tip, height).saturating_sub(1),
            x1: cell(centre.0, width).saturating_add(reach).min(width),
            y1: cell(centre.1, height).saturating_add(reach).min(height),
        };
        let mut dab = Dab {
            spent: 0.0,
            picked: bristle.colour,
            picked_weight: 0.0,
            area,
        };
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let reach_here =
                    1.0 - ((float(x) + 0.5 - centre.0).hypot(float(y) + 0.5 - centre.1) / tip);
                let clip = pass.clip.map_or(1.0, |clip| clip.get(x, y));
                if reach_here <= 0.0 || clip <= 0.0 {
                    continue;
                }
                // A loaded brush fills the weave; a dry one catches only its
                // high points.
                let tooth = self.canvas.ground.at(x, y);
                let catch = smoothstep(0.45 - bristle.load * 0.9, 0.95 - bristle.load * 0.9, tooth);
                let deposit =
                    (reach_here.min(1.0) * clip * catch * bristle.load.min(1.0) * pass.opacity)
                        .clamp(0.0, 1.0);
                if deposit <= 0.0 {
                    continue;
                }
                let index = at(width, x, y);
                let wet_here = read(&self.wet, width, x, y, 0.0);
                let cover_here = read(&self.cover, width, x, y, 0.0);
                let under = self.colour_at(x, y);
                let sticky = wet_here * cover_here * deposit;
                dab.picked = dab
                    .picked
                    .lerp(under, sticky / (dab.picked_weight + sticky).max(1e-6));
                dab.picked_weight += sticky;
                let colour = if cover_here > 0.0 {
                    under.lerp(bristle.colour, deposit)
                } else {
                    bristle.colour
                };
                self.write(index, colour, cover_here + (1.0 - cover_here) * deposit);
                // Each bristle leaves its own ridge; the light reads them as
                // the stroke's direction.
                if let Some(value) = self.thickness.get_mut(index) {
                    let ridge =
                        (0.15 + body) * (0.35 + 1.3 * bristle.ridge) * bristle.load.min(1.2);
                    *value += (ridge - *value) * (deposit * 0.7);
                }
                if let Some(value) = self.wet.get_mut(index) {
                    *value = value.max(deposit.min(1.0));
                }
                dab.spent += deposit;
            }
        }
        dab
    }

    fn colour_at(&self, x: usize, y: usize) -> Rgb {
        let width = self.width();
        Rgb::new(
            read(self.channel(0), width, x, y, 0.0),
            read(self.channel(1), width, x, y, 0.0),
            read(self.channel(2), width, x, y, 0.0),
        )
    }

    fn channel(&self, channel: usize) -> &[f32] {
        self.colour.get(channel).map_or(&[], Vec::as_slice)
    }

    fn write(&mut self, index: usize, colour: Rgb, cover: f32) {
        for (field, value) in self.colour.iter_mut().zip(colour.channels()) {
            if let Some(cell) = field.get_mut(index) {
                *cell = value;
            }
        }
        if let Some(cell) = self.cover.get_mut(index) {
            *cell = cover.clamp(0.0, 1.0);
        }
    }

    /// A thin coat scrubbed into the whole region first, as a painter lays
    /// in a passage before working it, so brushwork never leaves bare
    /// ground between its strokes.
    fn scrub(&mut self, settings: &Settings, region: &Fill, coverage: &Coverage, seed: u64) {
        let width = self.width();
        let area = coverage.area;
        let load = settings.get("load").min(1.0);
        let opacity = region.opacity.clamp(0.0, 1.0) * region.paint.alpha;
        let grain = region.brush.max(2.0) * 1.5;
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let inside = coverage.get(x, y);
                if inside <= 0.0 {
                    continue;
                }
                let tooth = self.canvas.ground.at(x, y);
                let catch = smoothstep(0.5 - load, 0.8 - load, tooth);
                let deposit = (inside * catch * opacity * (0.55 + 0.4 * load)).clamp(0.0, 1.0);
                let index = at(width, x, y);
                let cover = read(&self.cover, width, x, y, 0.0);
                let under = self.colour_at(x, y);
                let wander = fbm(float(x) / grain, float(y) / grain, seed ^ 0x5c4b, 3);
                let paint = region.paint.colour.scale(0.92 + 0.16 * wander);
                let colour = if cover > 0.0 {
                    under.lerp(paint, deposit)
                } else {
                    paint
                };
                self.write(index, colour, cover + (1.0 - cover) * deposit);
                if let Some(value) = self.thickness.get_mut(index) {
                    *value = value.max(deposit * 0.08);
                }
                if let Some(value) = self.wet.get_mut(index) {
                    *value = value.max(deposit);
                }
            }
        }
        self.wet_area = self.wet_area.union(area);
    }

    /// The brushwork laying in a region: parallel strokes along `angle`,
    /// broken into lengths and clipped to the outline.
    fn lay_in(&mut self, settings: &Settings, region: &Fill, seed: u64) {
        let brush = region.brush.max(2.0);
        let coverage = fill(
            &region.contours,
            self.canvas.width,
            self.canvas.height,
            brush,
            false,
        );
        if coverage.is_empty() {
            return;
        }
        self.scrub(settings, region, &coverage, seed);
        let area = coverage.area;
        let (cx, cy) = (
            float(area.x0.saturating_add(area.x1)) * 0.5,
            float(area.y0.saturating_add(area.y1)) * 0.5,
        );
        let radius = float(area.width()).hypot(float(area.height())) * 0.5 + brush;
        let (dy, dx) = region.angle.to_radians().sin_cos();
        let (nx, ny) = (-dy, dx);
        let spacing = brush * 0.45;
        let rows = cell(radius * 2.0 / spacing, 4096).saturating_add(1);
        let load = settings.get("load");
        for row in 0..rows {
            let across = float(row) * spacing - radius;
            let mut along = -radius - hash01(seed, 23, key(row)) * brush * 4.0;
            // Each piece advances at least 2.4 brush widths.
            let pieces = cell(radius * 2.0 / (brush * 2.4) + 6.0, 1 << 20);
            for piece in (0..pieces).map(key) {
                if along >= radius {
                    break;
                }
                let span = brush * (3.0 + 5.0 * hash01(seed, 29, mix(key(row)) ^ piece));
                let wobble = (hash01(seed, 31, mix(key(row)) ^ piece) - 0.5) * spacing * 0.5;
                let lean = (hash01(seed, 37, mix(key(row)) ^ piece) - 0.5) * 0.12;
                let start = (
                    cx + dx * along + nx * (across + wobble),
                    cy + dy * along + ny * (across + wobble),
                );
                let end_along = along + span;
                let end = (
                    cx + dx * end_along + nx * (across + wobble + span * lean),
                    cy + dy * end_along + ny * (across + wobble + span * lean),
                );
                let points = [
                    crate::brush::StrokePoint::new(start.0, start.1, 0.8),
                    crate::brush::StrokePoint::new(
                        (start.0 + end.0) * 0.5,
                        (start.1 + end.1) * 0.5,
                        1.0,
                    ),
                    crate::brush::StrokePoint::new(end.0, end.1, 0.85),
                ];
                let samples = smooth(&points, 0.6);
                let piece_seed = mix(seed ^ mix(key(row)) ^ piece.rotate_left(17));
                // Each brushload is mixed a little differently on the
                // palette: slightly lighter or darker, warmer or cooler.
                let value = 0.88 + 0.24 * hash01(piece_seed, 41, 0);
                let warmth = (hash01(piece_seed, 43, 0) - 0.5) * 0.08;
                let base = region.paint.colour.scale(value);
                let colour =
                    Rgb::new(base.r * (1.0 + warmth), base.g, base.b * (1.0 - warmth)).clamp();
                self.brush(
                    settings,
                    &Pass {
                        samples: &samples,
                        size: brush,
                        colour,
                        opacity: region.opacity.clamp(0.0, 1.0) * region.paint.alpha,
                        load,
                        seed: piece_seed,
                        clip: Some(&coverage),
                    },
                );
                along = end_along - brush * 0.6;
            }
        }
    }
}

impl LayerSim for OilLayer {
    fn apply(&mut self, mark: &Mark, seed: u64) {
        let settings = self.settings.with(mark.params());
        if !mark.wet() {
            self.dry();
        }
        match mark {
            Mark::Stroke(stroke) => {
                let samples = smooth(&stroke.points, 0.5);
                self.brush(
                    &settings,
                    &Pass {
                        samples: &samples,
                        size: stroke.size.max(0.5),
                        colour: stroke.paint.colour,
                        opacity: stroke.opacity.clamp(0.0, 1.0) * stroke.paint.alpha,
                        load: settings.get("load"),
                        seed,
                        clip: None,
                    },
                );
            }
            Mark::Fill(region) => self.lay_in(&settings, region, seed),
        }
    }

    fn composite(&self) -> Vec<u8> {
        let width = self.width();
        let relief = self.relief;
        let mut rgba = vec![0_u8; self.cover.len().saturating_mul(4)];
        rgba.par_chunks_mut(width.saturating_mul(4).max(4))
            .enumerate()
            .for_each(|(y, row)| {
                for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                    let cover = read(&self.cover, width, x, y, 0.0);
                    if cover <= 0.0 {
                        pixel.copy_from_slice(&[0, 0, 0, 0]);
                        continue;
                    }
                    // The surface is the paint over the weave: thin paint
                    // follows the threads, thick paint buries them.
                    let surface = |x: usize, y: usize, fallback: f32| {
                        if x >= width || y >= self.canvas.height {
                            return fallback;
                        }
                        let paint = read(&self.thickness, width, x, y, 0.0);
                        paint + self.canvas.ground.at(x, y) * 0.3 * (1.0 - paint.min(1.0))
                    };
                    let here = surface(x, y, 0.0);
                    let left = surface(x.saturating_sub(1), y, here);
                    let up = surface(x, y.saturating_sub(1), here);
                    let right = surface(x.saturating_add(1), y, here);
                    let down = surface(x, y.saturating_add(1), here);
                    // Light from the upper left: a slope rising toward it is lit.
                    let slope = (right - left) + (down - up);
                    let lit = (1.0 - relief * 0.45 * slope).clamp(0.6, 1.35);
                    let sheen = (-slope * relief * 0.5 - 0.25).max(0.0) * 0.35;
                    let colour = Rgb::new(
                        read(self.channel(0), width, x, y, 0.0),
                        read(self.channel(1), width, x, y, 0.0),
                        read(self.channel(2), width, x, y, 0.0),
                    )
                    .scale(lit);
                    let shine = |value: f32| from_linear(value + sheen * (1.0 - value));
                    pixel.copy_from_slice(&[
                        encode8(shine(colour.r)),
                        encode8(shine(colour.g)),
                        encode8(shine(colour.b)),
                        encode8(cover),
                    ]);
                }
            });
        rgba
    }

    fn clone_box(&self) -> Box<dyn LayerSim> {
        Box::new(self.clone())
    }
}
