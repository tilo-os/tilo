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
use crate::field::ColourField;
use crate::grid::{Area, at, box_blur, cell, float, key, read, smoothstep};
use crate::ground::{fbm, hash01, mix, value_noise};
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
                ParamSpec::new(
                    "glaze",
                    "Glaze",
                    "How transparent the paint is: a glaze tints what is beneath (dark over light shows most) and leaves no ridges.",
                    (0.0, 1.0),
                    0.0,
                ),
                ParamSpec::new(
                    "blender",
                    "Blender",
                    "A soft dry brush that lays no paint: it drags the wet paint beneath along its direction, softening edges and strokes; more is stronger. Blend across bands (angle 90) to melt them together.",
                    (0.0, 1.0),
                    0.0,
                ),
                ParamSpec::new(
                    "feather",
                    "Feathered edge",
                    "How far, in brush widths, a filled shape's edge is worked soft into what is beside it.",
                    (0.0, 4.0),
                    0.0,
                ),
                ParamSpec::new(
                    "edge",
                    "Broken edge",
                    "How irregularly a filled shape's edge is painted: the brush overshoots and falls short of the outline and breaks dry over the tooth there. 0 is a clean, drawn edge; 1 wanders half a brush width.",
                    (0.0, 1.0),
                    0.25,
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
        "Opaque paint with body: bristle strokes that streak and run dry, blend into wet paint on the same layer, cover dry paint and stand in lit ridges. Lights go on thick, darks thin; a sky is laid in thin and blended."
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
    /// How much lighter or darker this bristle's paint is than the brush's.
    shade: f32,
    /// How high a ridge it leaves, `0..=1`.
    ridge: f32,
    /// How far into the stroke it lands, and how far before the end it
    /// lifts: bristles meet the canvas unevenly, so ends are ragged.
    land: f32,
    lift: f32,
}

/// What one bristle dab laid and picked up.
struct Dab {
    /// The share of a full deposit the bristle gives up here.
    laying: f32,
    picked: Rgb,
    picked_weight: f32,
    area: Area,
}

/// One bristle's tip on the canvas at one sample of a stroke.
struct Touch {
    centre: (f32, f32),
    tip: f32,
    /// How firmly the bristle meets the canvas here, `0..=1`.
    contact: f32,
    /// How hard the hand presses, `0..=1`.
    pressure: f32,
    /// The unit direction the brush is travelling.
    heading: (f32, f32),
    /// How much paint heaps up here, `0..=1`: where a loaded brush lands
    /// and, most, where it lifts.
    pile: f32,
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
    /// How unevenly the bristles land and lift, in brush widths.
    ragged: f32,
    /// How much paint heaps where the brush lands and lifts: a stroke laid
    /// on its own heaps fully, the many short strokes of a lay-in little.
    heap: f32,
    /// The colour the brush is remixed toward as it travels, scaled by how
    /// this brushload was mixed.
    field: Option<(&'a ColourField, f32)>,
    /// How transparent the paint is, and what it lies over.
    glaze: Option<&'a Glaze>,
}

/// One brushstroke of a region's lay-in, before it is painted.
struct Piece {
    points: [crate::brush::StrokePoint; 3],
    size: f32,
    load: f32,
    colour: Rgb,
    /// How much lighter or darker than the region's colour it was mixed.
    value: f32,
    seed: u64,
}

/// A glaze being laid: how transparent it is, how strongly one coat
/// filters, and the paint beneath as it was before this coat. Its strokes
/// overlap, but one coat filters the paint beneath once, however often a
/// brush passes over it; a deeper glaze is a second coat.
struct Glaze {
    amount: f32,
    strength: f32,
    area: Area,
    beneath: Vec<Rgb>,
}

impl Glaze {
    /// The coat a mark lays over `area` of `layer`, if its paint is
    /// transparent at all.
    fn over(layer: &OilLayer, amount: f32, opacity: f32, area: Area) -> Option<Self> {
        if amount <= 0.0 || area.is_empty() {
            return None;
        }
        let beneath = (area.y0..area.y1)
            .flat_map(|y| (area.x0..area.x1).map(move |x| (x, y)))
            .map(|(x, y)| layer.colour_at(x, y))
            .collect();
        Some(Self {
            amount,
            strength: (0.8 * opacity).clamp(0.0, 1.0),
            area,
            beneath,
        })
    }

    fn beneath(&self, x: usize, y: usize) -> Option<Rgb> {
        let area = self.area;
        if x < area.x0 || y < area.y0 || x >= area.x1 || y >= area.y1 {
            return None;
        }
        let local = at(
            area.width(),
            x.saturating_sub(area.x0),
            y.saturating_sub(area.y0),
        );
        self.beneath.get(local).copied()
    }
}

/// How much body paint of `colour` has: lights are mixed with lead white
/// and laid thick, darks are lean and lie thin.
fn body_of(colour: Rgb) -> f32 {
    let luma = (0.2126 * colour.r + 0.7152 * colour.g + 0.0722 * colour.b).max(0.0);
    0.5 + 0.9 * luma.sqrt()
}

/// `under` seen through a coat of `paint`: a glaze filters the light, so it
/// darkens toward its own colour and never lightens.
fn glazed(under: Rgb, paint: Rgb, strength: f32) -> Rgb {
    let filter = Rgb::WHITE.lerp(paint, strength.clamp(0.0, 1.0));
    Rgb::new(under.r * filter.r, under.g * filter.g, under.b * filter.b)
}

/// The colour and cover of cell `(x, y)` once `deposit` of `paint` lands on
/// paint of colour `under` covering `cover` of it, through `glaze` when the
/// new paint is transparent.
fn coat(
    (under, cover): (Rgb, f32),
    paint: Rgb,
    deposit: f32,
    glaze: Option<&Glaze>,
    (x, y): (usize, usize),
) -> (Rgb, f32) {
    let opaque = if cover > 0.0 {
        under.lerp(paint, deposit)
    } else {
        paint
    };
    let opaque_cover = cover + (1.0 - cover) * deposit;
    let Some(glaze) = glaze else {
        return (opaque, opaque_cover);
    };
    // Over bare ground a glaze is a veil the ground shows through.
    let veil = cover + (1.0 - cover) * deposit * 0.35;
    let filtered = if cover > 0.0 {
        let beneath = glaze.beneath(x, y).unwrap_or(under);
        under.lerp(glazed(beneath, paint, glaze.strength), deposit)
    } else {
        paint
    };
    (
        opaque.lerp(filtered, glaze.amount),
        opaque_cover + (veil - opaque_cover) * glaze.amount,
    )
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
    fn bristles(settings: &Settings, pass: &Pass<'_>, scale: f32) -> Vec<Bristle> {
        // A thin brush has fewer bristles than its preset says: a line two
        // cells wide cannot hold twenty streaks.
        let count = cell(settings.get("bristles").min(pass.size * 0.9), 65).max(3);
        let streak = settings.get("streak");
        // Hairs clump: neighbouring bristles share their paint and their
        // ridge, so a stroke carries a few broad tracks of lighter and
        // darker, warmer and cooler paint, and the light picks out a few
        // ridges rather than a comb of hairlines.
        let clumps = (2.0 + pass.size / 7.0).min(8.0);
        (0..count)
            .map(|index| {
                let along = (float(index) + 0.5) / float(count);
                let clump =
                    |salt: f32| value_noise(along * clumps, salt * 7.31, pass.seed ^ 0x51c3);
                let jitter = hash01(pass.seed, 11, key(index)) - 0.5;
                let offset = (along * 2.0 - 1.0 + jitter / float(count)).clamp(-1.0, 1.0);
                let uneven = hash01(pass.seed, 13, key(index));
                // How far, in cells, a full bristle travels before it is dry.
                // However finely the canvas is simulated, a brushload lasts as
                // far over the artwork.
                let reach = pass.size * (10.0 + 30.0 * (1.0 - streak * uneven)) + 80.0 * scale;
                let shade = 1.0
                    + ((hash01(pass.seed, 19, key(index)) - 0.5) * 0.05 + (clump(0.0) - 0.5) * 0.2)
                        * (0.5 + streak);
                // Paint is never mixed through on the palette: each clump
                // carries a little more of one pigment or the other, warmer
                // or cooler, so a stroke is broken colour, not one flat hue.
                let warmth = ((hash01(pass.seed, 37, key(index)) - 0.5) * 0.04
                    + (clump(1.0) - 0.5) * 0.12)
                    * (0.5 + streak);
                let colour = pass.colour.scale(shade);
                let colour = Rgb::new(
                    colour.r * (1.0 + warmth),
                    colour.g,
                    colour.b * (1.0 - warmth),
                );
                // Outer bristles touch down later and lift sooner than the
                // middle of the brush, so an end narrows as well as frays;
                // the middle meets the canvas at once, so strokes laid end
                // to end still join.
                let edge = offset.abs();
                let ragged = pass.size * pass.ragged * (0.15 + 0.85 * edge);
                Bristle {
                    offset,
                    load: pass.load * (1.0 - streak * 0.6 * uneven),
                    spend: 1.0 / (reach * (0.6 + 0.8 * hash01(pass.seed, 17, key(index)))),
                    colour: colour.clamp(),
                    shade,
                    ridge: clump(2.0) * 0.65 + hash01(pass.seed, 23, key(index)) * 0.35,
                    land: ragged * (0.3 + hash01(pass.seed, 29, key(index))),
                    lift: ragged * (0.3 + hash01(pass.seed, 31, key(index))),
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
        let mut bristles = Self::bristles(settings, pass, self.canvas.scale);
        let blend = settings.get("blend");
        let tip = (pass.size / float(bristles.len().max(1)) * 1.3).max(0.9);
        // The paint a full deposit under a bristle's cone-shaped tip lays.
        let footprint = (std::f32::consts::PI * tip * tip / 3.0).max(0.5);
        let body = settings.get("thickness");
        // How far along the stroke a bristle's contact stays alike: a dry
        // brush skips in streaks this long, not dot by dot over the weave.
        let streak_length = (pass.size * 2.0).max(5.0);
        let patch_size = (pass.size * 1.5).max(6.0);
        let lanes = float(bristles.len()) * 0.35;
        // Wet paint beneath is taken up by the distance dragged over the
        // artwork, not by how many cells that is.
        let pickup = 0.04 / self.canvas.scale.max(0.05);
        // A short dab keeps most of its bristles however ragged the brush.
        let room = total * 0.3;
        let mut touched = Area::EMPTY;
        let mut previous: Option<(f32, f32)> = None;
        let mut mixed: Option<Rgb> = None;
        for sample in pass.samples {
            let travelled = previous.map_or(0.0, |(x, y)| (sample.x - x).hypot(sample.y - y));
            previous = Some((sample.x, sample.y));
            // Across a graded passage the painter keeps remixing the brush
            // toward the colour wanted where it is, so the bristles follow
            // the gradation and keep whatever wet paint they picked up.
            if let Some((field, value)) = pass.field {
                let here = field.at(sample.x, sample.y).scale(value);
                if let Some(before) = mixed {
                    for bristle in &mut bristles {
                        let shade = bristle.shade;
                        let colour = bristle.colour;
                        bristle.colour = Rgb::new(
                            colour.r + (here.r - before.r) * shade,
                            colour.g + (here.g - before.g) * shade,
                            colour.b + (here.b - before.b) * shade,
                        )
                        .clamp();
                    }
                }
                mixed = Some(here);
            }
            let half = pass.size
                * 0.5
                * (0.35 + 0.65 * sample.pressure)
                * taper(sample.distance, total, pass.size);
            // The hand rides up and bears down over a few brush widths, so
            // the whole brush catches in patches as well as in streaks.
            let (px, py) = self.canvas.absolute(sample.x, sample.y);
            let patch = fbm(px / patch_size, py / patch_size, pass.seed ^ 0x2e61, 2);
            let pile = pass.heap
                * (0.4 * (1.0 - smoothstep(0.0, pass.size * 0.6, sample.distance))
                    + 0.9 * (1.0 - smoothstep(0.0, pass.size * 0.5, total - sample.distance)));
            for bristle in &mut bristles {
                if bristle.load <= 0.002
                    || sample.distance < bristle.land.min(room)
                    || total - sample.distance < bristle.lift.min(room)
                {
                    continue;
                }
                let streak = fbm(
                    sample.distance / streak_length,
                    bristle.offset * lanes,
                    pass.seed ^ 0x7a3d,
                    2,
                );
                // The outer hairs splay and gather as the brush travels, so
                // a stroke's sides fray instead of running ruler-straight.
                let splay = bristle.offset + (streak - 0.5) * bristle.offset.abs();
                let centre = (
                    sample.x - sample.ty * splay * half,
                    sample.y + sample.tx * splay * half,
                );
                let contact = smoothstep(0.25, 0.75, streak * 0.65 + patch * 0.35);
                let touch = Touch {
                    centre,
                    tip,
                    contact,
                    pressure: sample.pressure,
                    heading: (sample.tx, sample.ty),
                    pile,
                };
                let dab = self.dab(bristle, &touch, body, pass);
                // Wet paint beneath sticks to the bristle and travels with
                // it, by how far the bristle dragged through it.
                if dab.picked_weight > 0.0 {
                    let share = (dab.picked_weight / footprint).min(1.0);
                    bristle.colour = bristle
                        .colour
                        .lerp(dab.picked, (blend * share * travelled * pickup).min(0.5));
                }
                // A bristle spends paint by the distance it drags, at the
                // rate it lays it: pressed lightly or running dry, it lays less.
                let laying = dab.laying.min(1.0);
                bristle.load = (bristle.load - travelled * laying * bristle.spend).max(0.0);
                touched = touched.union(dab.area);
            }
        }
        self.wet_area = self.wet_area.union(touched);
    }

    /// One bristle's tip pressed on the canvas as `touch` says.
    fn dab(&mut self, bristle: &Bristle, touch: &Touch, body: f32, pass: &Pass<'_>) -> Dab {
        let (width, height) = (self.width(), self.canvas.height);
        let (centre, tip) = (touch.centre, touch.tip);
        let dryness = 1.0 - smoothstep(0.2, 0.6, bristle.load);
        // A hard-pressed dry brush is pushed down into the weave; a light
        // one skims the tops, the more so where the bristle lifts.
        let press =
            (0.5 + (touch.contact - 0.5) * dryness) * 0.6 + (touch.pressure - 0.6) * 0.18 * dryness;
        let reach = cell(tip.ceil() + 1.0, 64);
        let area = Area {
            x0: cell(centre.0 - tip, width).saturating_sub(1),
            y0: cell(centre.1 - tip, height).saturating_sub(1),
            x1: cell(centre.0, width).saturating_add(reach).min(width),
            y1: cell(centre.1, height).saturating_add(reach).min(height),
        };
        let mut dab = Dab {
            laying: 0.0,
            picked: bristle.colour,
            picked_weight: 0.0,
            area,
        };
        // How much of its load the bristle gives up here, as a share of a
        // full deposit: what it would lay over bare ground of average
        // tooth. It follows the brush alone, not the paint beneath, so a
        // window of the painting spends the brush exactly as the whole
        // canvas does, wherever its strokes enter it.
        {
            let grip = press + 0.11;
            let catch = smoothstep(0.45 - bristle.load * 0.9, 0.95 - bristle.load * 0.9, grip);
            let thinned = pass.glaze.map_or(0.0, |glaze| glaze.amount);
            let catch =
                (catch + (1.0 - catch) * thinned) * (1.0 - thinned * 0.4 * (1.0 - touch.contact));
            dab.laying = catch * bristle.load.min(1.0) * pass.opacity.clamp(0.0, 1.0);
        }
        if area.x0 >= area.x1 || area.y0 >= area.y1 {
            return dab;
        }
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let reach_here =
                    1.0 - ((float(x) + 0.5 - centre.0).hypot(float(y) + 0.5 - centre.1) / tip);
                let clip = pass.clip.map_or(1.0, |clip| clip.get(x, y));
                if reach_here <= 0.0 || clip <= 0.0 {
                    continue;
                }
                // A loaded brush fills the weave; a dry one catches where the
                // bristle presses and the threads stand high, and on the
                // faces of the threads it drags against more than their lee,
                // so broken colour streaks along the stroke.
                // Over paint that has body, the tooth is the paint's own
                // ridges: a dry brush dragged across impasto catches its
                // crests and skips its grooves, and the weave it buries
                // no longer shows.
                let ground = &self.canvas.ground;
                let buried = read(&self.thickness, width, x, y, 0.0).min(1.0);
                let mut tooth =
                    ground.at(x, y) * (1.0 - buried) + 0.5 * buried + self.crest(x, y) * 1.5;
                if dryness > 0.0 {
                    tooth += ground.rise(x, y, touch.heading) * 1.2 * dryness * (1.0 - buried);
                }
                // A loaded brush floods the weave; only as it runs dry does
                // the tooth decide where it catches, so overlapping colours
                // do not print the weave as a grid.
                let grip = (tooth - 0.5) * 0.22 * (0.25 + 0.75 * dryness) + press + 0.11;
                let catch = smoothstep(0.45 - bristle.load * 0.9, 0.95 - bristle.load * 0.9, grip);
                // A glaze is thinned to flow: it settles into the weave
                // instead of catching on the threads, but its bristles still
                // drag it out in streaks along the stroke.
                let thinned = pass.glaze.map_or(0.0, |glaze| glaze.amount);
                let catch = (catch + (1.0 - catch) * thinned)
                    * (1.0 - thinned * 0.4 * (1.0 - touch.contact));
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
                let (colour, cover) = coat(
                    (under, cover_here),
                    bristle.colour,
                    deposit,
                    pass.glaze,
                    (x, y),
                );
                // However many strokes of a lay-in cross a cell its outline
                // only grazes, they cover no more of it than the outline
                // does, so the edge keeps its smoothness.
                let cover = if clip < 1.0 {
                    cover.min(cover_here.max(clip))
                } else {
                    cover
                };
                self.write(index, colour, cover);
                // Each bristle leaves its own ridge; the light reads them as
                // the stroke's direction. A glaze is too thin to leave one.
                if let Some(value) = self.thickness.get_mut(index) {
                    // The outer hairs push paint aside into a rim, and the
                    // paint heaps where the brush lands and lifts.
                    let rim = smoothstep(0.6, 1.0, bristle.offset.abs()) * 0.6 * pass.heap;
                    let ridge = (0.15 + body)
                        * (0.35 + 1.3 * bristle.ridge)
                        * bristle.load.min(1.2)
                        * body_of(bristle.colour)
                        * (1.0 + rim + touch.pile * bristle.load.min(1.0));
                    let body_share = 1.0 - pass.glaze.map_or(0.0, |glaze| glaze.amount);
                    *value += (ridge - *value) * (deposit * 0.7 * body_share);
                }
                // Fresh oil stays workable however thinly it went on: any
                // real touch of paint leaves the cell wet.
                if let Some(value) = self.wet.get_mut(index) {
                    *value = value.max(smoothstep(0.0, 0.3, deposit));
                }
            }
        }
        dab
    }

    /// How far the paint at `(x, y)` stands above the paint around it.
    fn crest(&self, x: usize, y: usize) -> f32 {
        let width = self.width();
        let here = read(&self.thickness, width, x, y, 0.0);
        if here <= 0.0 {
            return 0.0;
        }
        let around = [
            (x.saturating_sub(2), y),
            (x.saturating_add(2), y),
            (x, y.saturating_sub(2)),
            (x, y.saturating_add(2)),
        ]
        .iter()
        .map(|&(nx, ny)| read(&self.thickness, width, nx, ny, here))
        .sum::<f32>()
            * 0.25;
        here - around
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
    fn scrub(
        &mut self,
        (settings, glaze): (&Settings, Option<&Glaze>),
        region: &Fill,
        coverage: &Coverage,
        seed: u64,
    ) {
        let width = self.width();
        let area = coverage.area;
        let load = settings.get("load").min(1.0);
        let transparency = glaze.map_or(0.0, |glaze| glaze.amount);
        let body = settings.get("thickness");
        let opacity = region.opacity.clamp(0.0, 1.0) * region.paint.alpha;
        let grain = region.brush.max(2.0) * 1.5;
        for y in area.y0..area.y1 {
            for x in area.x0..area.x1 {
                let inside = coverage.get(x, y);
                if inside <= 0.0 {
                    continue;
                }
                let tooth = self.canvas.ground.at(x, y);
                let (ax, ay) = self.canvas.absolute(float(x), float(y));
                let wander = fbm(ax / grain, ay / grain, seed ^ 0x5c4b, 3);
                // A thin coat is scrubbed on unevenly, so where it misses the
                // weave it misses in patches, not thread by thread.
                let catch = smoothstep(0.5 - load, 0.75 - load, tooth * 0.5 + wander * 0.5);
                let catch = catch + (1.0 - catch) * transparency;
                let deposit = (inside * catch * opacity * (0.7 + 0.35 * load)).clamp(0.0, 1.0);
                let index = at(width, x, y);
                let cover = read(&self.cover, width, x, y, 0.0);
                let under = self.colour_at(x, y);
                let wanted = region
                    .field
                    .as_ref()
                    .map_or(region.paint.colour, |field| field.at(float(x), float(y)));
                let paint = wanted.scale(0.92 + 0.16 * wander);
                // Into wet paint the two mix; over dry paint this covers.
                let wet_under = read(&self.wet, width, x, y, 0.0) * cover;
                let mixing = settings.get("blend") * wet_under * 0.8;
                let laid = if cover > 0.0 {
                    deposit * (1.0 - mixing)
                } else {
                    deposit
                };
                let (colour, _) = coat((under, cover), paint, laid, glaze, (x, y));
                let (_, covered) = coat((under, cover), paint, deposit, glaze, (x, y));
                self.write(index, colour, covered);
                // The scrubbed coat already has most of the passage's body,
                // so the strokes worked into it leave ridges, not cliffs at
                // every stroke's edge.
                if let Some(value) = self.thickness.get_mut(index) {
                    *value = value
                        .max(deposit * (0.15 + body) * 0.6 * (1.0 - transparency) * body_of(paint));
                }
                if let Some(value) = self.wet.get_mut(index) {
                    *value = value.max(smoothstep(0.0, 0.3, deposit));
                }
            }
        }
        self.wet_area = self.wet_area.union(area);
    }

    /// The brushwork laying in a region: parallel strokes along `angle`,
    /// broken into lengths and clipped to the outline.
    fn lay_in(&mut self, settings: &Settings, region: &Fill, seed: u64) {
        let Some(coverage) = self.region_coverage(settings, region, seed) else {
            return;
        };
        let opacity = region.opacity.clamp(0.0, 1.0) * region.paint.alpha;
        let glaze = Glaze::over(self, settings.get("glaze"), opacity, coverage.area);
        let glaze = glaze.as_ref();
        // Where the gradient fades the paint out, the passage is worked
        // thinner, however many of its strokes cross there.
        let fading = region
            .field
            .as_ref()
            .filter(|field| field.stops.iter().any(|stop| stop.2 < 1.0));
        let before: Option<Vec<CellState>> = fading.map(|_| {
            let area = coverage.area;
            (area.y0..area.y1)
                .flat_map(|y| (area.x0..area.x1).map(move |x| (x, y)))
                .map(|(x, y)| self.state(x, y))
                .collect()
        });
        self.scrub((settings, glaze), region, &coverage, seed);
        // The brushwork is laid out over the whole region, wherever this
        // grid's window of it lies, so every window paints the same pieces.
        let feather = settings.get("feather") * region.brush.max(2.0);
        let Some(extent) = extent(&region.contours, region.brush.max(2.0) + feather) else {
            return;
        };
        let mut pieces = Self::pieces(region, extent, settings.get("load"), seed);
        let (width, height) = (float(self.canvas.width), float(self.canvas.height));
        // Pieces wholly off this grid leave nothing on it.
        pieces.retain(|piece| {
            let reach = piece.size + 2.0;
            let [first, ..] = piece.points;
            let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
            for point in &piece.points {
                (x0, y0) = (x0.min(point.x), y0.min(point.y));
                (x1, y1) = (x1.max(point.x), y1.max(point.y));
            }
            x1 > -reach && y1 > -reach && x0 < width + reach && y0 < height + reach
        });
        // A painter works a passage back and forth rather than row by row,
        // so later strokes cross earlier ones anywhere in it.
        pieces.sort_by(|a, b| hash01(a.seed, 47, 0).total_cmp(&hash01(b.seed, 47, 0)));
        for piece in pieces {
            let samples = smooth(&piece.points, 0.6);
            self.brush(
                settings,
                &Pass {
                    samples: &samples,
                    size: piece.size,
                    colour: piece.colour,
                    opacity,
                    load: piece.load,
                    seed: piece.seed,
                    clip: Some(&coverage),
                    ragged: 0.6,
                    heap: 0.1,
                    field: region.field.as_ref().map(|field| (field, piece.value)),
                    glaze,
                },
            );
        }
        if let (Some(field), Some(before)) = (fading, before) {
            self.fade_back(coverage.area, &before, field);
        }
    }

    /// Return each cell of `area` toward its state in `before` as far as
    /// `field` fades the paint out there.
    fn fade_back(&mut self, area: Area, before: &[CellState], field: &ColourField) {
        let width = self.width();
        for (local, old) in before.iter().enumerate() {
            let (x, y) = area.position(local);
            let keep = 1.0 - field.alpha(float(x) + 0.5, float(y) + 0.5);
            if keep <= 0.0 {
                continue;
            }
            let now = self.state(x, y);
            let mixed: Vec<f32> = now
                .iter()
                .zip(old)
                .map(|(new, old)| new + (old - new) * keep)
                .collect();
            let index = at(width, x, y);
            if let [red, green, blue, cover, thickness, wet] = mixed[..] {
                // The colour of thinned paint is still its own: only its
                // cover gives way to what is beneath.
                let colour = if old[3] > 0.0 {
                    Rgb::new(red, green, blue)
                } else {
                    Rgb::new(now[0], now[1], now[2])
                };
                self.write(index, colour, cover);
                if let Some(slot) = self.thickness.get_mut(index) {
                    *slot = thickness;
                }
                if let Some(slot) = self.wet.get_mut(index) {
                    *slot = wet;
                }
            }
        }
    }

    /// Where a region's paint goes, its edge softened as far as it is worked
    /// into what is beside it.
    fn region_coverage(&self, settings: &Settings, region: &Fill, seed: u64) -> Option<Coverage> {
        let brush = region.brush.max(2.0);
        let feather = settings.get("feather") * brush;
        let coverage = fill(
            &region.contours,
            self.canvas.width,
            self.canvas.height,
            brush + feather,
            false,
        );
        if coverage.is_empty() {
            return None;
        }
        let area = coverage.area;
        let (span, rows) = (area.width(), area.height());
        let values = self.broken(&coverage, brush, settings.get("edge"), seed);
        // Paint laid wet into wet is worked into its neighbour: its edge
        // feathers over half a brush instead of stopping at the outline.
        let values = if region.wet {
            box_blur(&values, span, rows, cell(brush * 0.5, 256).max(1))
        } else {
            values
        };
        // A feathered edge is worked out further, and twice over, so it
        // fades without a visible ramp.
        let values = if feather >= 1.0 {
            let radius = cell(feather * 0.5, 1024).max(1);
            box_blur(&box_blur(&values, span, rows, radius), span, rows, radius)
        } else {
            values
        };
        Some(Coverage { area, values })
    }

    /// `coverage` with its outline painted by hand: within `edge` half
    /// brush widths of it the paint overshoots and falls short over a few
    /// brush widths, and breaks dry where the tooth stands high.
    fn broken(&self, coverage: &Coverage, brush: f32, edge: f32, seed: u64) -> Vec<f32> {
        let reach = edge * brush * 0.5;
        if reach < 0.5 {
            return coverage.values.clone();
        }
        let area = coverage.area;
        let radius = cell(reach, 256).max(1);
        let soft = box_blur(&coverage.values, area.width(), area.height(), radius);
        // However little the edge wanders, it stays at least a cell soft,
        // so a thin brush's outline does not step from cell to cell.
        let window = (0.6 / float(radius.saturating_mul(2).saturating_add(1))).max(0.1);
        let wander = (brush * 1.5).max(3.0);
        soft.iter()
            .enumerate()
            .map(|(local, &value)| {
                if value <= 0.0 || value >= 1.0 {
                    return value;
                }
                let (x, y) = area.position(local);
                let (ax, ay) = self.canvas.absolute(float(x), float(y));
                let along = fbm(ax / wander, ay / wander, seed ^ 0x3e11, 3);
                let grit = self.canvas.ground.at(x, y);
                let threshold =
                    (0.5 + (along - 0.5) * 1.1 + (grit - 0.5) * 0.5 * edge).clamp(0.1, 0.85);
                smoothstep(threshold - window, threshold + window, value)
            })
            .collect()
    }

    /// The strokes laying in `area`: overlapping lengths roughly along
    /// `region.angle`, each a different length, width, lean, curve and
    /// brushload, landing and lifting rather than starting at full width.
    fn pieces(region: &Fill, extent: [f32; 4], load: f32, seed: u64) -> Vec<Piece> {
        let brush = region.brush.max(2.0);
        let [x0, y0, x1, y1] = extent;
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        let radius = (x1 - x0).hypot(y1 - y0) * 0.5 + brush;
        let spacing = brush * 0.45;
        let rows = cell(radius * 2.0 / spacing, 4096).saturating_add(1);
        let mut pieces = Vec::new();
        for row in 0..rows {
            let row_key = mix(key(row));
            let across = float(row) * spacing - radius;
            let mut along = -radius - hash01(seed, 23, row_key) * brush * 4.0;
            // Each piece advances at least 1.25 brush widths.
            let most = cell(radius * 2.0 / (brush * 1.25) + 6.0, 1 << 20);
            for piece in (0..most).map(key) {
                if along >= radius {
                    break;
                }
                let at = |salt: u64| hash01(seed, salt, row_key ^ piece);
                let span = brush * (2.5 + 6.0 * at(29));
                let wobble = (at(31) - 0.5) * spacing;
                let angle = region.angle.to_radians() + (at(37) - 0.5) * 0.3;
                let (dy, dx) = angle.sin_cos();
                let (nx, ny) = (-dy, dx);
                let bow = (at(39) - 0.5) * span * 0.16;
                let start = (
                    cx + dx * along + nx * (across + wobble),
                    cy + dy * along + ny * (across + wobble),
                );
                let point = |t: f32, sideways: f32, pressure: f32| {
                    crate::brush::StrokePoint::new(
                        start.0 + dx * span * t + nx * sideways,
                        start.1 + dy * span * t + ny * sideways,
                        pressure,
                    )
                };
                let piece_seed = mix(seed ^ row_key ^ piece.rotate_left(17));
                // Each brushload is mixed a little differently on the
                // palette: slightly lighter or darker, warmer or cooler.
                let value = 0.93 + 0.14 * hash01(piece_seed, 41, 0);
                let warmth = (hash01(piece_seed, 43, 0) - 0.5) * 0.06;
                let base = region
                    .field
                    .as_ref()
                    .map_or(region.paint.colour, |field| field.at(start.0, start.1))
                    .scale(value);
                pieces.push(Piece {
                    points: [
                        point(0.0, 0.0, 0.45 + 0.3 * at(51)),
                        point(0.5, bow, 1.0),
                        point(1.0, 0.0, 0.4 + 0.35 * at(53)),
                    ],
                    size: brush * (0.75 + 0.45 * at(57)),
                    load: load * (0.75 + 0.45 * at(59)),
                    colour: Rgb::new(base.r * (1.0 + warmth), base.g, base.b * (1.0 - warmth))
                        .clamp(),
                    value,
                    seed: piece_seed,
                });
                // Overlap by a quarter to a half of the piece, more than its
                // frayed ends give up, so the lay-in leaves no holes.
                along += span * (0.5 + 0.25 * at(61));
            }
        }
        pieces
    }
}

/// Which way a blender drags at each cell.
enum Directions {
    Constant((f32, f32)),
    /// Per cell of `area`, row-major; zero where the brush never passed.
    Field {
        area: Area,
        values: Vec<(f32, f32)>,
    },
}

impl Directions {
    /// The way a brush following `samples` travels near each cell it
    /// passes within `radius` of.
    fn along(samples: &[Sample], radius: f32, area: Area) -> Self {
        let mut values = vec![(0.0, 0.0); area.width().saturating_mul(area.height())];
        let reach = radius.max(1.0);
        for sample in samples {
            let x0 = cell(sample.x - reach, area.x1).max(area.x0);
            let y0 = cell(sample.y - reach, area.y1).max(area.y0);
            let x1 = cell(sample.x + reach + 1.0, area.x1.saturating_add(1)).min(area.x1);
            let y1 = cell(sample.y + reach + 1.0, area.y1.saturating_add(1)).min(area.y1);
            for y in y0..y1 {
                for x in x0..x1 {
                    let local = at(
                        area.width(),
                        x.saturating_sub(area.x0),
                        y.saturating_sub(area.y0),
                    );
                    if let Some(slot) = values.get_mut(local) {
                        *slot = (sample.tx, sample.ty);
                    }
                }
            }
        }
        Self::Field { area, values }
    }

    fn at(&self, x: usize, y: usize) -> (f32, f32) {
        match self {
            Self::Constant(direction) => *direction,
            Self::Field { area, values } => {
                if x < area.x0 || y < area.y0 || x >= area.x1 || y >= area.y1 {
                    return (0.0, 0.0);
                }
                let local = at(
                    area.width(),
                    x.saturating_sub(area.x0),
                    y.saturating_sub(area.y0),
                );
                values.get(local).copied().unwrap_or((0.0, 0.0))
            }
        }
    }
}

/// The light on paint whose surface rises by `(gx, gy)` per cell, lit from
/// the upper left as the ground is: how much brighter or darker the slope
/// is than flat paint, and the sheen on crests of paint with body. Oil
/// dries glossy, so a ridge of thick paint facing the light catches a
/// pale highlight and its far side falls into shadow.
fn light((gx, gy): (f32, f32), relief: f32, thick: f32) -> (f32, f32) {
    // The light's direction, normalised: from the upper left, well above.
    const LIGHT: (f32, f32, f32) = (-0.48, -0.56, 0.675);
    let steep = relief * 1.6;
    let (nx, ny) = (-gx * steep, -gy * steep);
    let length = (nx * nx + ny * ny + 1.0).sqrt();
    let facing = (nx * LIGHT.0 + ny * LIGHT.1 + LIGHT.2) / length;
    let lit = (facing / LIGHT.2).clamp(0.55, 1.4);
    let gloss = smoothstep(0.15, 0.8, thick) * relief;
    let sheen = smoothstep(1.05, 1.3, lit) * gloss * 0.3;
    (lit, sheen)
}

/// What a cell holds: linear colour, cover, thickness and wetness.
type CellState = [f32; 6];

/// How a blender drags: taps either side of a cell, the distance between
/// them, the width of its hair lanes, its strength and seed.
#[derive(Clone, Copy)]
struct Kernel {
    taps: usize,
    step: f32,
    lane: f32,
    strength: f32,
    seed: u64,
}

impl OilLayer {
    fn state(&self, x: usize, y: usize) -> CellState {
        let width = self.width();
        [
            read(self.channel(0), width, x, y, 0.0),
            read(self.channel(1), width, x, y, 0.0),
            read(self.channel(2), width, x, y, 0.0),
            read(&self.cover, width, x, y, 0.0),
            read(&self.thickness, width, x, y, 0.0),
            read(&self.wet, width, x, y, 0.0),
        ]
    }

    /// The state between cells, mixed from the four around the position.
    fn sample(&self, x: f32, y: f32) -> CellState {
        let (width, height) = (self.width(), self.canvas.height);
        let x = (x - 0.5).clamp(0.0, float(width.saturating_sub(1)));
        let y = (y - 0.5).clamp(0.0, float(height.saturating_sub(1)));
        let (cx, cy) = (cell(x, width), cell(y, height));
        let (fx, fy) = (x - float(cx), y - float(cy));
        let (nx, ny) = (
            cx.saturating_add(1).min(width.saturating_sub(1)),
            cy.saturating_add(1).min(height.saturating_sub(1)),
        );
        let corners = [
            (self.state(cx, cy), (1.0 - fx) * (1.0 - fy)),
            (self.state(nx, cy), fx * (1.0 - fy)),
            (self.state(cx, ny), (1.0 - fx) * fy),
            (self.state(nx, ny), fx * fy),
        ];
        let mut mixed = [0.0; 6];
        for (state, weight) in corners {
            for (slot, value) in mixed.iter_mut().zip(state) {
                *slot += value * weight;
            }
        }
        mixed
    }

    /// A soft dry brush worked through the wet paint under `coverage`:
    /// each cell takes on the paint it is dragged through along
    /// `directions`, up to `reach` either side. Dry paint holds; bare ground
    /// takes paint dragged onto it, so a softened edge fades out.
    fn soften(
        &mut self,
        coverage: &Coverage,
        directions: &Directions,
        reach: f32,
        strength: f32,
        seed: u64,
    ) {
        let area = coverage.area;
        if area.is_empty() || strength <= 0.0 {
            return;
        }
        let taps = cell(reach, 9).clamp(2, 8);
        let kernel = Kernel {
            taps,
            step: reach.max(0.5) / float(taps),
            lane: (reach * 0.12).max(1.5),
            strength,
            seed,
        };
        let this = &*self;
        let rows: Vec<Vec<CellState>> = (area.y0..area.y1)
            .into_par_iter()
            .map(|y| {
                (area.x0..area.x1)
                    .map(|x| this.softened((x, y), coverage, directions, &kernel))
                    .collect()
            })
            .collect();
        let width = self.width();
        for (y, row) in (area.y0..area.y1).zip(rows) {
            for (x, state) in (area.x0..area.x1).zip(row) {
                let index = at(width, x, y);
                let [r, g, b, cover, thickness, wet] = state;
                self.write(index, Rgb::new(r, g, b), cover);
                if let Some(slot) = self.thickness.get_mut(index) {
                    *slot = thickness;
                }
                if let Some(slot) = self.wet.get_mut(index) {
                    *slot = wet;
                }
            }
        }
        self.wet_area = self.wet_area.union(area);
    }

    /// One cell of [`Self::soften`].
    fn softened(
        &self,
        (x, y): (usize, usize),
        coverage: &Coverage,
        directions: &Directions,
        kernel: &Kernel,
    ) -> CellState {
        let Kernel {
            taps,
            step,
            lane: lane_size,
            strength,
            seed,
        } = *kernel;
        let here = self.state(x, y);
        let [red, green, blue, cover, thickness, wet] = here;
        let before = Rgb::new(red, green, blue);
        let inside = coverage.get(x, y);
        let mobile = if cover > 0.0 { wet } else { 1.0 };
        let (dx, dy) = directions.at(x, y);
        if inside <= 0.0 || mobile <= 0.0 || (dx == 0.0 && dy == 0.0) {
            return here;
        }
        let (px, py) = (float(x) + 0.5, float(y) + 0.5);
        let mut paint = [0.0_f32; 3];
        let (mut covered, mut weight, mut body) = (0.0_f32, 0.0_f32, 0.0_f32);
        let reach = float(taps.saturating_add(1));
        for tap in 0..=taps.saturating_mul(2) {
            let offset = float(tap) - float(taps);
            // The hairs fan out across the stroke as well, so a band laid
            // along it softens at its sides too; mirrored taps keep the
            // kernel centred, so softening never shifts the paint.
            let fan = if tap == taps {
                0.0
            } else {
                (offset.abs() * 0.618_034).fract().mul_add(2.0, -1.0) * offset.signum()
            };
            let across = fan * step * float(taps) * 0.35;
            let [sr, sg, sb, s_cover, s_thickness, s_wet] = self.sample(
                px + dx * offset * step - dy * across,
                py + dy * offset * step + dx * across,
            );
            let moves = if s_cover > 0.0 { s_wet } else { 1.0 };
            let share = moves * (1.0 - offset.abs() / reach);
            weight += share;
            covered += share * s_cover;
            body += share * s_thickness;
            for (slot, value) in paint.iter_mut().zip([sr, sg, sb]) {
                *slot += share * s_cover * value;
            }
        }
        if weight <= 0.0 {
            return here;
        }
        // The hairs of a blender drag in lanes, so even a softened passage
        // keeps a grain along the stroke: some lanes carry the paint
        // through, others barely move it.
        let (ax, ay) = self.canvas.absolute(px, py);
        let across = ax * -dy + ay * dx;
        let along = ax * dx + ay * dy;
        let lane = 0.35
            + 0.65
                * smoothstep(
                    0.2,
                    0.8,
                    fbm(
                        across / lane_size,
                        along / (lane_size * 12.0),
                        seed ^ 0x6b1d,
                        2,
                    ),
                );
        // Each hair leaves a groove in the paint it combs, finer than the
        // lanes, which the light picks out as the blender's direction.
        let groove = value_noise(
            across / (lane_size * 0.6),
            along / (lane_size * 10.0),
            seed ^ 0x6b1e,
        );
        let pull = (strength * inside * mobile * lane).clamp(0.0, 1.0);
        let mean_cover = covered / weight;
        let new_cover = cover + (mean_cover - cover) * pull;
        let colour = if covered > 1e-6 {
            let mean = Rgb::new(paint[0] / covered, paint[1] / covered, paint[2] / covered);
            if cover > 0.0 {
                before.lerp(mean, pull)
            } else {
                mean
            }
        } else {
            before
        };
        // Blending levels the ridges of the strokes beneath and combs its
        // own into what is left.
        let levelled = (thickness + (body / weight - thickness) * pull) * (1.0 - 0.4 * pull);
        let new_thickness = levelled + pull * 0.05 * groove * mean_cover;
        let new_wet = if new_cover > 0.0 { wet.max(pull) } else { wet };
        [
            colour.r,
            colour.g,
            colour.b,
            new_cover,
            new_thickness,
            new_wet,
        ]
    }

    /// A blender worked over a region or along a stroke instead of paint.
    fn blend_mark(&mut self, settings: &Settings, mark: &Mark, seed: u64) {
        let strength = settings.get("blender");
        match mark {
            Mark::Fill(region) => {
                let Some(coverage) = self.region_coverage(settings, region, seed) else {
                    return;
                };
                let (sin, cos) = region.angle.to_radians().sin_cos();
                let reach = region.brush.max(2.0) * 1.2;
                self.soften(
                    &coverage,
                    &Directions::Constant((cos, sin)),
                    reach,
                    strength,
                    seed,
                );
            }
            Mark::Stroke(stroke) => {
                let samples = smooth(&stroke.points, 0.5);
                let size = stroke.size.max(1.0);
                let coverage = crate::mask::brush_footprint(
                    &samples,
                    size,
                    0.7,
                    (self.canvas.width, self.canvas.height),
                    size,
                );
                let directions = Directions::along(&samples, size * 0.75, coverage.area);
                let reach = (size * 0.75).max(2.0);
                self.soften(&coverage, &directions, reach, strength, seed);
            }
        }
    }
}

impl LayerSim for OilLayer {
    fn apply(&mut self, mark: &Mark, seed: u64) {
        let settings = self.settings.with(mark.params());
        // A blender only works paint that is still wet.
        if settings.get("blender") > 0.0 {
            self.blend_mark(&settings, mark, seed);
            return;
        }
        if !mark.wet() {
            self.dry();
        }
        match mark {
            Mark::Stroke(stroke) => {
                let samples = smooth(&stroke.points, 0.5);
                let size = stroke.size.max(0.5);
                let opacity = stroke.opacity.clamp(0.0, 1.0) * stroke.paint.alpha;
                let mut min = (f32::INFINITY, f32::INFINITY);
                let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
                for sample in &samples {
                    min = (min.0.min(sample.x), min.1.min(sample.y));
                    max = (max.0.max(sample.x), max.1.max(sample.y));
                }
                let reach =
                    Area::around(min, max, size + 2.0, self.canvas.width, self.canvas.height);
                let glaze = Glaze::over(self, settings.get("glaze"), opacity, reach);
                let clip = (!stroke.clip.is_empty()).then(|| {
                    fill(
                        &stroke.clip,
                        self.canvas.width,
                        self.canvas.height,
                        1.0,
                        false,
                    )
                });
                self.brush(
                    &settings,
                    &Pass {
                        samples: &samples,
                        size,
                        colour: stroke.paint.colour,
                        opacity,
                        load: settings.get("load"),
                        seed,
                        clip: clip.as_ref(),
                        ragged: 0.9,
                        // A dab is pressed on, not dragged: only a stroke
                        // drawn out several brush widths ploughs up a rim
                        // and leaves a heap where it lifts.
                        heap: smoothstep(size, size * 4.0, length(&samples)),
                        field: None,
                        glaze: glaze.as_ref(),
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
                        paint + self.canvas.ground.at(x, y) * 0.05 * (1.0 - paint.min(1.0))
                    };
                    let here = surface(x, y, 0.0);
                    let left = surface(x.saturating_sub(1), y, here);
                    let up = surface(x, y.saturating_sub(1), here);
                    let right = surface(x.saturating_add(1), y, here);
                    let down = surface(x, y.saturating_add(1), here);
                    let thick = read(&self.thickness, width, x, y, 0.0);
                    // Slopes per artwork unit, so a ridge catches the light
                    // alike however finely the layer is simulated.
                    let per_unit = self.canvas.scale * 0.5;
                    let (lit, sheen) = light(
                        ((right - left) * per_unit, (down - up) * per_unit),
                        relief,
                        thick,
                    );
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

/// The cells a region's outline spans grown by `margin`, as
/// [`Area::around`] would give it on a grid large enough to hold it: the
/// same on every window of the grid, up to the window's origin.
fn extent(contours: &[Vec<(f32, f32)>], margin: f32) -> Option<[f32; 4]> {
    let mut min = (f32::INFINITY, f32::INFINITY);
    let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for &(x, y) in contours.iter().flatten() {
        if x.is_finite() && y.is_finite() {
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        }
    }
    let margin = margin.max(1.0);
    min.0.is_finite().then(|| {
        [
            (min.0 - margin).floor(),
            (min.1 - margin).floor(),
            (max.0 + margin).ceil() + 1.0,
            (max.1 + margin).ceil() + 1.0,
        ]
    })
}
