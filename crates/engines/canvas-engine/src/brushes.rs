//! The artwork glue over the shared stroke core: a mark's points, size,
//! preset and dynamics turned into the plain inputs [`tilo_paint_strokes`]
//! lays, and the twigs and dabs a stroke's `forks` and `dabs` grow out of
//! its id.
//!
//! The geometry itself — the outline a line of ink fills, the dabs a soft
//! brush stamps, smoothing and simplification — is in `tilo-paint-strokes`,
//! below both this engine and the shared paint surface, so a stroke is
//! previewed while it is drawn by the same code that bakes it.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "float geometry, and pixel indices bounded by the raster they index"
)]

use std::collections::BTreeMap;

use tilo_wet_media::StrokePoint as WetPoint;
use vello::kurbo::{Affine, BezPath, Point};

use crate::model::{Mark, Shape, Style};

pub use tilo_paint_strokes::{
    Brush, DYNAMICS, Dabs, Dynamics, Piece, Preset, centreline, corners, kept, problem, samples,
    simplified, smoothed, steady_ends, stored,
};

/// The preset a style draws with.
pub fn preset_of(style: &Style) -> Preset {
    style
        .brush
        .as_deref()
        .and_then(Preset::parse)
        .unwrap_or_default()
}

/// The dynamics a mark's style sets.
pub fn dynamics_of(mark: &Mark) -> Dynamics {
    Dynamics::of(preset_of(&mark.style), &mark.style.params)
}

/// The plain brush a style draws with.
pub fn brush_of(style: &Style) -> Brush {
    Brush::of(preset_of(style), &style.params, style.angle)
}

/// The catalogue id of a preset's name.
pub fn label(preset: Preset) -> String {
    format!("canvas-brush-{}", preset.name())
}

/// The blend a preset mixes with when the mark names none: a marker's
/// overlaps darken.
pub fn default_blend(style: &Style) -> Option<&'static str> {
    (preset_of(style) == Preset::Marker).then_some("multiply")
}

/// The outline a stroke drawn with an outline preset fills, in the mark's
/// own units; for a dabbed preset, the region its dabs cover.
pub fn silhouette(pieces: &[Piece], style: &Style) -> BezPath {
    tilo_paint_strokes::silhouette(pieces, &brush_of(style))
}

/// How see-through a preset's outline is laid, before the mark's opacity.
pub fn ink_alpha(style: &Style) -> f64 {
    tilo_paint_strokes::ink_alpha(&brush_of(style))
}

pub struct DabInput<'a> {
    pub pieces: &'a [Piece],
    pub style: &'a Style,
    /// Pixels per unit wanted.
    pub scale: f64,
    /// The paint's colour, `[r, g, b]`.
    pub rgb: [u8; 3],
    /// The ground's tooth at a point of the artwork, `0..=1`, mean a half.
    pub tooth: &'a dyn Fn(Point) -> f64,
    /// The mark's own units to the artwork's.
    pub to_artwork: Affine,
}

pub fn dab(input: &DabInput<'_>) -> Option<Dabs> {
    tilo_paint_strokes::dab(&tilo_paint_strokes::DabInput {
        pieces: input.pieces,
        brush: &brush_of(input.style),
        scale: input.scale,
        rgb: input.rgb,
        tooth: input.tooth,
        to_artwork: input.to_artwork,
    })
}

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
        .filter(|_| preset_of(&mark.style) != Preset::Calligraphy);
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

pub fn stroke_of(mark: &Mark) -> Option<(&[Vec<f64>], f64)> {
    match &mark.shape {
        Shape::Stroke { points, size, .. } => Some((points.as_slice(), *size)),
        _ => None,
    }
}

fn point_of(point: &[f64]) -> (f64, f64, f64) {
    (
        point.first().copied().unwrap_or(0.0),
        point.get(1).copied().unwrap_or(0.0),
        point.get(2).copied().unwrap_or(1.0),
    )
}

fn f32_of_f64(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(brush: &str, forks: Option<u32>, dabs: Option<u32>) -> Mark {
        Mark {
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
            style: Style {
                brush: stored(brush),
                ..Style::default()
            },
            transform: None,
            group: String::new(),
            clip: String::new(),
            area: String::new(),
        }
    }

    fn flat(_: Point) -> f64 {
        0.5
    }

    tilo_test_env::scenario! {
        /// What a stroke's forks and dabs grow into, from the mark alone.
        /// Each case was a test of its own and builds its own fixture; a failure names its phase.
        fn a_stroke_grows_forks_and_dabs_from_its_shape;

        phase every_preset_grows_forks_and_dabs;
        {
            for preset in ["pen", "marker", "calligraphy"] {
                let plain = stroke(preset, None, None);
                let tree = stroke(preset, Some(3), None);
                let leaves = stroke(preset, None, Some(40));
                let plain = silhouette(&pieces(&plain), &plain.style);
                let tree = silhouette(&pieces(&tree), &tree.style);
                let leaves = silhouette(&pieces(&leaves), &leaves.style);
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
                    let mark = stroke(preset, forks, None);
                    dab(&DabInput {
                        pieces: &pieces(&mark),
                        style: &mark.style,
                        scale: 1.0,
                        rgb: [20, 20, 20],
                        tooth: &flat,
                        to_artwork: Affine::IDENTITY,
                    })
                    .expect("laid")
                };
                assert!(laid(Some(3)).width > laid(None).width * 2, "{preset} forks");
            }
        }
    }
}
