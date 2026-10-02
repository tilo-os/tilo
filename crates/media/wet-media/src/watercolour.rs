//! Transparent watercolour.
//!
//! Each cell holds water, pigment suspended in it and pigment already
//! deposited, as optical density per channel (Beer–Lambert: a wash lets
//! `exp(-density)` of the light through). A mark wets its footprint with an
//! irregular edge and loads pigment into it; pigment then flows through
//! whatever is wet, so a stroke laid into a wet wash bleeds. When the wash
//! dries, water evaporating fastest at its rim carries pigment there (the
//! dark edge of a dried wash), and granulating pigments settle into the
//! paper's valleys. Layers composite with multiply, as glazes do.

use std::sync::Arc;

use rayon::prelude::*;

use crate::brush::{ink_outline, smooth};
use crate::colour::{encode8, from_linear};
use crate::engine::{Blend, Canvas, LayerSim, Mark, MediumEngine, ParamSpec, Settings};
use crate::grid::{Area, at, box_blur, cell, extract, float, smoothstep, store};
use crate::ground::fbm;
use crate::mask::{Coverage, fill};

/// The watercolour engine.
pub struct Watercolour {
    params: Arc<[ParamSpec]>,
}

impl Watercolour {
    pub fn new() -> Self {
        Self {
            params: vec![
                ParamSpec::new(
                    "water",
                    "Water",
                    "How wet the brush is; wetter washes spread further and dry with a darker edge.",
                    (0.0, 1.0),
                    0.6,
                ),
                ParamSpec::new(
                    "flow",
                    "Flow",
                    "How far pigment travels through wet paint; high flow bleeds wet-in-wet.",
                    (0.0, 1.0),
                    0.5,
                ),
                ParamSpec::new(
                    "edge",
                    "Edge darkening",
                    "How much pigment gathers at the rim of a drying wash.",
                    (0.0, 1.0),
                    0.6,
                ),
                ParamSpec::new(
                    "granulation",
                    "Granulation",
                    "How strongly pigment settles into the paper's tooth.",
                    (0.0, 1.0),
                    0.5,
                ),
                ParamSpec::new(
                    "roughness",
                    "Roughness",
                    "How irregular a wash's edge and density are.",
                    (0.0, 1.0),
                    0.5,
                ),
                ParamSpec::new(
                    "load",
                    "Pigment load",
                    "How much pigment the brush carries; 1 is a medium wash, 2 near masstone.",
                    (0.05, 2.0),
                    0.7,
                ),
            ]
            .into(),
        }
    }
}

impl Default for Watercolour {
    fn default() -> Self {
        Self::new()
    }
}

impl MediumEngine for Watercolour {
    fn id(&self) -> &'static str {
        "watercolour"
    }

    fn label(&self) -> &'static str {
        "Watercolour"
    }

    fn description(&self) -> &'static str {
        "Transparent washes that bleed while wet, dry with dark edges and granulate on textured paper. Light comes from the paper: paint darkens, never lightens."
    }

    fn parameters(&self) -> Arc<[ParamSpec]> {
        Arc::clone(&self.params)
    }

    fn blend(&self) -> Blend {
        Blend::Multiply
    }

    fn new_layer(&self, canvas: &Canvas, settings: Settings) -> Box<dyn LayerSim> {
        let cells = canvas.width.saturating_mul(canvas.height);
        Box::new(WaterLayer {
            canvas: canvas.clone(),
            settings,
            water: vec![0.0; cells],
            suspended: std::array::from_fn(|_| vec![0.0; cells]),
            granulating: vec![0.0; cells],
            deposit: std::array::from_fn(|_| vec![0.0; cells]),
            wet_area: Area::EMPTY,
            edge: 0.6,
            granulation: 0.5,
        })
    }
}

#[derive(Clone)]
struct WaterLayer {
    canvas: Canvas,
    settings: Settings,
    water: Vec<f32>,
    /// Optical density of pigment still moving, per channel.
    suspended: [Vec<f32>; 3],
    /// How granulating the suspended pigment is, `0..=1`.
    granulating: Vec<f32>,
    /// Optical density of pigment fixed to the paper, per channel.
    deposit: [Vec<f32>; 3],
    /// The cells holding water.
    wet_area: Area,
    /// Drying settings of the wet wash, from the mark that last wetted it.
    edge: f32,
    granulation: f32,
}

impl WaterLayer {
    fn width(&self) -> usize {
        self.canvas.width
    }

    fn footprint(&self, mark: &Mark, size: f32, margin: f32) -> Coverage {
        let (width, height) = (self.canvas.width, self.canvas.height);
        match mark {
            Mark::Stroke(stroke) => {
                let samples = smooth(&stroke.points, 0.75);
                let outline = ink_outline(&samples, size);
                fill(&[outline], width, height, margin, false)
            }
            Mark::Fill(region) => fill(&region.contours, width, height, margin, false),
        }
    }

    /// The deposit once everything wet has dried, without changing the state.
    fn dried(&self) -> [Vec<f32>; 3] {
        let mut deposit = self.deposit.clone();
        self.dry_into(&mut deposit);
        deposit
    }

    fn dry_into(&self, deposit: &mut [Vec<f32>; 3]) {
        let area = self.wet_area;
        if area.is_empty() {
            return;
        }
        let width = self.width();
        let wet: Vec<f32> = extract(&self.water, width, area)
            .into_iter()
            .map(|water| smoothstep(0.02, 0.25, water))
            .collect();
        let rim_radius = cell((3.0 + self.edge * 7.0) * self.canvas.scale.max(0.5), 64).max(1);
        let around = box_blur(&wet, area.width(), area.height(), rim_radius);
        let granulating = extract(&self.granulating, width, area);
        let settle: Vec<f32> = (0..wet.len())
            .map(|index| {
                let (x, y) = area.position(index);
                // Water evaporates fastest at the rim and draws pigment
                // there, so the rim darkens and the middle pales a little.
                let rim = ((wet.get(index).copied().unwrap_or(0.0)
                    - around.get(index).copied().unwrap_or(0.0))
                    * 2.0)
                    .clamp(0.0, 1.0);
                let rim = rim * rim.sqrt();
                let tooth = self.canvas.ground.at(x, y);
                let grain = granulating.get(index).copied().unwrap_or(0.0) * self.granulation;
                (1.0 - self.edge * 0.2 + self.edge * 2.6 * rim)
                    * (1.0 + grain * (0.5 - tooth) * 2.6).max(0.15)
            })
            .collect();
        for (channel, deposit) in self.suspended.iter().zip(deposit.iter_mut()) {
            let moving = extract(channel, width, area);
            let mut fixed = extract(deposit, width, area);
            for ((fixed, moving), settle) in fixed.iter_mut().zip(&moving).zip(&settle) {
                *fixed += moving * settle;
            }
            store(deposit, width, area, &fixed);
        }
    }

    fn dry(&mut self) {
        if self.wet_area.is_empty() {
            return;
        }
        let mut deposit = std::mem::take(&mut self.deposit);
        self.dry_into(&mut deposit);
        self.deposit = deposit;
        let area = self.wet_area;
        let width = self.width();
        let zero = vec![0.0; area.width().saturating_mul(area.height())];
        store(&mut self.water, width, area, &zero);
        store(&mut self.granulating, width, area, &zero);
        for channel in &mut self.suspended {
            store(channel, width, area, &zero);
        }
        self.wet_area = Area::EMPTY;
    }
}

impl LayerSim for WaterLayer {
    fn apply(&mut self, mark: &Mark, seed: u64) {
        let settings = self.settings.with(mark.params());
        if !mark.wet() {
            self.dry();
        }
        let (paint, opacity, size) = match mark {
            Mark::Stroke(stroke) => (stroke.paint, stroke.opacity, stroke.size.max(0.5)),
            Mark::Fill(region) => (region.paint, region.opacity, region.brush.max(4.0)),
        };
        let water = settings.get("water");
        let flow = settings.get("flow");
        let roughness = settings.get("roughness");
        let spread = 1.0 + flow * (1.0 + water) * (size * 0.06 + 1.5);
        let margin = spread * 3.0 + size * 0.1 + 4.0;
        let coverage = self.footprint(mark, size, margin);
        if coverage.is_empty() {
            return;
        }
        let area = coverage.area;
        let (span, rows) = (area.width(), area.height());
        let width = self.width();
        let outline = wash_edge(&coverage, size, roughness, seed);

        let mut local_water = extract(&self.water, width, area);
        let mut local_granulating = extract(&self.granulating, width, area);
        let load = settings.get("load") * opacity.clamp(0.0, 1.0) * paint.alpha * paint.strength;
        let granulation = paint.granulation.max(0.15);
        let grain = (size * 0.35).max(3.0);
        let density: Vec<f32> = outline
            .iter()
            .enumerate()
            .map(|(index, mask)| {
                let (x, y) = area.position(index);
                let (x, y) = (float(x), float(y));
                // Pigment pools unevenly: broad puddles and finer mottling.
                let bloom = fbm(x / (grain * 2.0), y / (grain * 2.0), seed ^ 0xb100, 2);
                let pool = fbm(x / (grain * 6.0), y / (grain * 6.0), seed ^ 0x9001, 2);
                mask * load
                    * (1.0 - roughness * 0.45 + roughness * 0.6 * bloom + roughness * 0.5 * pool)
            })
            .collect();
        for ((water_cell, mask), granulating) in local_water
            .iter_mut()
            .zip(&outline)
            .zip(local_granulating.iter_mut())
        {
            let before = *water_cell;
            *water_cell += mask * (0.3 + 0.7 * water);
            let added = mask * load;
            let total = before + added;
            if total > 1e-6 {
                *granulating = (*granulating * before + granulation * added) / total;
            }
        }

        // Flow: pigment and water move through whatever is wet, so a mark
        // laid into a wet wash bleeds into it.
        let wet: Vec<f32> = local_water
            .iter()
            .map(|water| smoothstep(0.02, 0.3, *water))
            .collect();
        let radius = cell(spread, 64).max(1);
        let passes = if flow > 0.05 { 3 } else { 1 };
        let mut moving: [Vec<f32>; 3] =
            std::array::from_fn(|channel| extract(self.suspended_channel(channel), width, area));
        let absorbance = paint
            .colour
            .channels()
            .map(|channel| -channel.clamp(0.004, 1.0).ln());
        for (channel, absorbance) in moving.iter_mut().zip(absorbance) {
            for (value, density) in channel.iter_mut().zip(&density) {
                *value += density * absorbance;
            }
        }
        for _ in 0..passes {
            for channel in &mut moving {
                let spread_out = box_blur(channel, span, rows, radius);
                for ((value, blurred), wet) in channel.iter_mut().zip(&spread_out).zip(&wet) {
                    *value += (blurred - *value) * wet * flow.max(0.15);
                }
            }
            let spread_water = box_blur(&local_water, span, rows, radius);
            for ((value, blurred), wet) in local_water.iter_mut().zip(&spread_water).zip(&wet) {
                *value += (blurred - *value) * wet * 0.5 * flow;
            }
        }
        for (index, channel) in moving.iter().enumerate() {
            if let Some(field) = self.suspended.get_mut(index) {
                store(field, width, area, channel);
            }
        }
        store(&mut self.water, width, area, &local_water);
        store(&mut self.granulating, width, area, &local_granulating);
        self.wet_area = self.wet_area.union(area);
        self.edge = settings.get("edge") * (0.4 + 0.6 * water);
        self.granulation = settings.get("granulation");
    }

    fn composite(&self) -> Vec<u8> {
        let deposit = self.dried();
        let width = self.width();
        let mut rgba = vec![0_u8; self.water.len().saturating_mul(4)];
        rgba.par_chunks_mut(width.saturating_mul(4).max(4))
            .enumerate()
            .for_each(|(y, row)| {
                for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
                    let index = at(width, x, y);
                    let encoded = std::array::from_fn::<f32, 3, _>(|channel| {
                        let density = deposit
                            .get(channel)
                            .and_then(|field| field.get(index))
                            .copied()
                            .unwrap_or(0.0);
                        from_linear((-density.max(0.0)).exp())
                    });
                    // Multiply by `encoded` is drawing `colour` at `alpha`
                    // over the backdrop with a multiply blend.
                    let darkest = encoded.iter().copied().fold(1.0_f32, f32::min);
                    let alpha = 1.0 - darkest;
                    let colour = encoded.map(|value| {
                        if alpha > 1e-4 {
                            (value - darkest) / alpha
                        } else {
                            1.0
                        }
                    });
                    pixel.copy_from_slice(&[
                        encode8(colour[0]),
                        encode8(colour[1]),
                        encode8(colour[2]),
                        encode8(alpha),
                    ]);
                }
            });
        rgba
    }

    fn clone_box(&self) -> Box<dyn LayerSim> {
        Box::new(self.clone())
    }
}

impl WaterLayer {
    fn suspended_channel(&self, channel: usize) -> &[f32] {
        self.suspended.get(channel).map_or(&[], Vec::as_slice)
    }
}

/// A wash's irregular, soft edge: the footprint blurred and cut at a
/// threshold that wanders with noise.
fn wash_edge(coverage: &Coverage, size: f32, roughness: f32, seed: u64) -> Vec<f32> {
    let area = coverage.area;
    let soften = cell(1.0 + roughness * size * 0.05, 32).max(1);
    let soft = box_blur(&coverage.values, area.width(), area.height(), soften);
    let grain = (size * 0.35).max(3.0);
    let wander = 0.55 * roughness;
    soft.iter()
        .enumerate()
        .map(|(index, value)| {
            let (x, y) = area.position(index);
            let (x, y) = (float(x), float(y));
            let threshold = 0.5 + (fbm(x / grain, y / grain, seed, 3) - 0.5) * wander;
            smoothstep(threshold - 0.12, threshold + 0.12, *value)
        })
        .collect()
}
