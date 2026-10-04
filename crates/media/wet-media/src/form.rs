//! Strokes a painter makes many of at once: a limb that forks into ever
//! thinner twigs, or a cluster of short dabs scattered along a path. Each
//! grows from the stroke and its seed alone, so a whole tree or a mass of
//! foliage is one mark that syncs, edits and undoes as one, and paints the
//! same on every machine.

use serde::{Deserialize, Serialize};

use crate::brush::{Sample, StrokePoint, length, smooth};
use crate::colour::Rgb;
use crate::engine::{Mark, Stroke};
use crate::grid::{float, key};
use crate::ground::{hash01, mix};

/// The most strokes one mark grows into.
pub const MOST_PIECES: usize = 600;

/// What a stroke grows into besides its own line.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Form {
    /// The stroke is a limb that lifts to a point and forks `forks` times
    /// into twigs, each thinner, splaying about `spread` degrees.
    Branch { forks: u32, spread: f32 },
    /// Instead of one line, `count` short dabs the stroke's width,
    /// scattered up to `scatter` cells either side of its path, each about
    /// `length` cells long (about its width when zero) and pointing along
    /// the path, or `angle` degrees when given. A dab much longer than it is
    /// wide is flicked: it lands full and lifts to a point, as a blade of
    /// grass is painted.
    Dabs {
        count: u32,
        scatter: f32,
        #[serde(default)]
        length: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        angle: Option<f32>,
    },
}

/// How one mark's dabs are laid.
#[derive(Clone, Copy)]
struct Dabbing {
    count: u32,
    scatter: f32,
    length: f32,
    angle: Option<f32>,
}

impl Form {
    /// This form with artwork units converted to cells.
    #[must_use]
    pub fn scaled(self, scale: f32) -> Self {
        match self {
            Self::Dabs {
                count,
                scatter,
                length,
                angle,
            } => Self::Dabs {
                count,
                scatter: scatter * scale,
                length: length * scale,
                angle,
            },
            branch @ Self::Branch { .. } => branch,
        }
    }
}

impl Stroke {
    /// The plain strokes this one is painted as, each with its seed: itself
    /// when it has no form.
    pub fn pieces(&self, seed: u64) -> Vec<(Stroke, u64)> {
        match self.form {
            None => vec![(self.clone(), seed)],
            Some(Form::Branch { forks, spread }) => branch(self, forks, spread, seed),
            Some(Form::Dabs {
                count,
                scatter,
                length,
                angle,
            }) => dabs(
                self,
                Dabbing {
                    count,
                    scatter,
                    length,
                    angle,
                },
                seed,
            ),
        }
    }
}

impl Mark {
    /// The plain marks this one is painted as, each with its seed.
    pub fn pieces(&self, seed: u64) -> Vec<(Mark, u64)> {
        match self {
            Self::Stroke(stroke) if stroke.form.is_some() => stroke
                .pieces(seed)
                .into_iter()
                .map(|(stroke, seed)| (Self::Stroke(stroke), seed))
                .collect(),
            mark => vec![(mark.clone(), seed)],
        }
    }
}

/// A piece of `stroke` along `points`, `size` wide, its paint lightened or
/// darkened by `value` and warmed or cooled by `warmth`.
fn piece(
    stroke: &Stroke,
    points: Vec<StrokePoint>,
    size: f32,
    (value, warmth): (f32, f32),
) -> Stroke {
    let colour = stroke.paint.colour.scale(value);
    Stroke {
        points,
        size,
        paint: crate::colour::Paint {
            colour: Rgb::new(
                colour.r * (1.0 + warmth),
                colour.g,
                colour.b * (1.0 - warmth),
            )
            .clamp(),
            ..stroke.paint
        },
        form: None,
        ..stroke.clone()
    }
}

/// A random unit value for piece `index` of a mark seeded `seed`.
fn chance(seed: u64, salt: u64, index: usize) -> f32 {
    hash01(seed, salt, key(index))
}

/// The point and heading `at` cells along `samples`.
fn along(samples: &[Sample], at: f32) -> Sample {
    let index = samples.partition_point(|sample| sample.distance < at);
    samples
        .get(index.min(samples.len().saturating_sub(1)))
        .copied()
        .unwrap_or(Sample {
            x: 0.0,
            y: 0.0,
            pressure: 1.0,
            tx: 1.0,
            ty: 0.0,
            distance: 0.0,
        })
}

/// One limb or twig still to grow: its path, its width where it starts,
/// and how many more times it forks.
struct Limb {
    samples: Vec<Sample>,
    size: f32,
    forks: u32,
    seed: u64,
}

/// A limb that tapers to a point and forks into twigs, and they into
/// thinner twigs, `forks` times over.
fn branch(stroke: &Stroke, forks: u32, spread: f32, seed: u64) -> Vec<(Stroke, u64)> {
    let forks = forks.min(6);
    let spread = spread.clamp(5.0, 90.0).to_radians();
    let mut out = Vec::new();
    let mut growing = vec![Limb {
        samples: smooth(&stroke.points, (stroke.size * 0.5).max(1.0)),
        size: stroke.size,
        forks,
        seed,
    }];
    while let Some(limb) = growing.pop() {
        if out.len() >= MOST_PIECES {
            break;
        }
        let total = length(&limb.samples);
        // The limb is painted as one stroke that lifts to a point: the hand
        // eases off as it goes, however the painter pressed it.
        let points: Vec<StrokePoint> = limb
            .samples
            .iter()
            .step_by(4)
            .chain(limb.samples.last())
            .map(|sample| {
                let t = if total > 0.0 {
                    sample.distance / total
                } else {
                    0.0
                };
                StrokePoint::new(sample.x, sample.y, sample.pressure * (1.0 - 0.8 * t))
            })
            .collect();
        let value = 0.96 + 0.08 * chance(limb.seed, 3, 0);
        out.push((piece(stroke, points, limb.size, (value, 0.0)), limb.seed));
        if limb.forks == 0 || limb.size < 1.2 || total < 2.0 {
            continue;
        }
        // A limb puts out side branches all along it, alternating sides,
        // more of them the longer it is, and forks again near its tip.
        let twigs = 2_usize
            .saturating_add(usize::from(chance(limb.seed, 5, 0) < 0.5))
            .saturating_add(usize::from(total > limb.size * 12.0))
            .saturating_add(usize::from(total > limb.size * 24.0));
        for twig in 0..twigs {
            let at = 0.18
                + 0.75 * (float(twig) + 0.25 + 0.5 * chance(limb.seed, 7, twig)) / float(twigs);
            let from = along(&limb.samples, total * at);
            let hand = if twig
                .saturating_add(usize::from(chance(limb.seed, 9, 0) < 0.5))
                .is_multiple_of(2)
            {
                1.0
            } else {
                -1.0
            };
            let mut angle = from.ty.atan2(from.tx)
                + hand * spread * (0.55 + 0.75 * chance(limb.seed, 11, twig));
            let reach = total * (0.4 + 0.3 * chance(limb.seed, 13, twig)) * (1.0 - 0.35 * at);
            let size =
                limb.size * (1.0 - 0.7 * at).max(0.2) * (0.6 + 0.25 * chance(limb.seed, 17, twig));
            // A twig starts inside its limb, so it grows out of it rather
            // than beside it, and bends a little as it goes.
            let mut x = from.x - from.tx * size * 0.5;
            let mut y = from.y - from.ty * size * 0.5;
            let mut points = vec![StrokePoint::new(x, y, 1.0)];
            let bends = 4;
            for bend in 0..bends {
                // Twigs kink where they grew a bud and turn a little toward
                // the light above as they go.
                angle += (chance(limb.seed, key(bend).saturating_add(19), twig) - 0.5) * 0.6;
                let toward_up = (-std::f32::consts::FRAC_PI_2 - angle + std::f32::consts::PI)
                    .rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                angle += toward_up * 0.1;
                let step = reach / float(bends);
                x += angle.cos() * step;
                y += angle.sin() * step;
                points.push(StrokePoint::new(x, y, 1.0));
            }
            let twig_seed = mix(limb.seed ^ key(twig).wrapping_add(1).rotate_left(29));
            growing.push(Limb {
                samples: smooth(&points, (size * 0.5).max(1.0)),
                size,
                forks: limb.forks.saturating_sub(1),
                seed: twig_seed,
            });
        }
    }
    out
}

/// Short dabs scattered along the stroke's path, each a little different
/// in size, direction, length and mix, as a painter dabs in foliage.
fn dabs(stroke: &Stroke, dabbing: Dabbing, seed: u64) -> Vec<(Stroke, u64)> {
    let count = usize::try_from(dabbing.count)
        .unwrap_or(MOST_PIECES)
        .min(MOST_PIECES);
    let samples = smooth(&stroke.points, 1.0);
    let total = length(&samples);
    let scatter = dabbing.scatter.max(0.0);
    let pointed = dabbing.angle.map(f32::to_radians);
    (0..count)
        .map(|index| {
            let roll = |salt: u64| chance(seed, salt, index);
            let centre = along(&samples, total * roll(3));
            // Two rolls summed fall mostly near the path and thin out away
            // from it, as a mass of dabs does at its edge.
            let across = (roll(5) + roll(7) - 1.0) * scatter;
            let lengthwise =
                (roll(11) + roll(13) - 1.0) * scatter * if total > 0.0 { 0.5 } else { 1.0 };
            let (x, y) = (
                centre.x - centre.ty * across + centre.tx * lengthwise,
                centre.y + centre.tx * across + centre.ty * lengthwise,
            );
            let size = stroke.size * (0.7 + 0.45 * roll(17));
            let angle = pointed.map_or_else(
                || centre.ty.atan2(centre.tx) + (roll(19) - 0.5) * 1.6,
                |angle| angle + (roll(19) - 0.5) * 0.7,
            );
            let reach = if dabbing.length > 0.0 {
                dabbing.length * (0.6 + 0.8 * roll(23))
            } else {
                stroke.size * (0.6 + 1.2 * roll(23))
            };
            let (dx, dy) = (angle.cos() * reach, angle.sin() * reach);
            // A long dab is flicked: full where it lands, lifting to a point,
            // and bending only a little, as a blade does.
            let flicked = reach > size * 3.0;
            let bow = (roll(29) - 0.5) * reach * if flicked { 0.12 } else { 0.4 };
            let pressure = centre.pressure;
            let (land, lift) = if flicked { (1.0, 0.12) } else { (0.7, 0.45) };
            let points = vec![
                StrokePoint::new(x, y, land * pressure),
                StrokePoint::new(
                    x + dx * 0.5 - dy / reach.max(1e-3) * bow,
                    y + dy * 0.5 + dx / reach.max(1e-3) * bow,
                    pressure,
                ),
                StrokePoint::new(x + dx, y + dy, lift * pressure),
            ];
            let mixed = (0.92 + 0.16 * roll(31), (roll(37) - 0.5) * 0.08);
            (
                piece(stroke, points, size, mixed),
                mix(seed ^ key(index).rotate_left(23)),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::colour::parse_paint;

    fn limb(form: Form) -> Stroke {
        Stroke {
            points: vec![
                StrokePoint::new(80.0, 150.0, 1.0),
                StrokePoint::new(82.0, 60.0, 1.0),
            ],
            size: 10.0,
            paint: parse_paint("#3a2e26", &[]).unwrap(),
            opacity: 1.0,
            wet: false,
            params: BTreeMap::new(),
            form: Some(form),
            clip: Vec::new(),
        }
    }

    #[test]
    fn a_branch_forks_into_thinner_twigs_that_lift_to_points() {
        let stroke = limb(Form::Branch {
            forks: 3,
            spread: 35.0,
        });
        let pieces = stroke.pieces(42);
        assert_eq!(
            pieces,
            stroke.pieces(42),
            "grows the same from the same seed"
        );
        assert_ne!(pieces, stroke.pieces(43));
        // 1 limb, 2-3 twigs, each 2-3 more, three levels deep.
        assert!((15..=40).contains(&pieces.len()), "{} pieces", pieces.len());
        assert!(pieces.iter().all(|(piece, _)| piece.form.is_none()));
        let thinnest = pieces.iter().map(|(p, _)| p.size).fold(f32::MAX, f32::min);
        assert!(thinnest < 4.0, "twigs thin out: {thinnest}");
        let (main, _) = &pieces[0];
        let last = main.points.last().unwrap();
        assert!(
            last.pressure < 0.3,
            "the limb lifts to a point: {}",
            last.pressure
        );
        // Twigs reach beyond the limb's own tip.
        let top = pieces
            .iter()
            .flat_map(|(p, _)| p.points.iter().map(|point| point.y))
            .fold(f32::MAX, f32::min);
        assert!(
            top < 60.0
                || pieces
                    .iter()
                    .any(|(p, _)| p.points.iter().any(|q| q.x < 70.0 || q.x > 92.0))
        );
    }

    #[test]
    fn dabs_scatter_along_the_path_each_mixed_a_little_differently() {
        let stroke = limb(Form::Dabs {
            count: 40,
            scatter: 20.0,
            length: 0.0,
            angle: None,
        });
        let pieces = stroke.pieces(7);
        assert_eq!(pieces.len(), 40);
        let colours: std::collections::BTreeSet<String> = pieces
            .iter()
            .map(|(p, _)| p.paint.colour.to_hex())
            .collect();
        assert!(colours.len() > 10, "{} distinct mixes", colours.len());
        for (dab, _) in &pieces {
            for point in &dab.points {
                assert!(
                    (50.0..=115.0).contains(&point.x),
                    "near the path: {}",
                    point.x
                );
                assert!((40.0..=175.0).contains(&point.y));
            }
            assert!(dab.size < 15.0);
        }
        let blades = Stroke {
            form: Some(Form::Dabs {
                count: 30,
                scatter: 20.0,
                length: 40.0,
                angle: Some(-90.0),
            }),
            size: 3.0,
            ..stroke.clone()
        }
        .pieces(7);
        for (blade, _) in &blades {
            let (root, tip) = (blade.points[0], blade.points[2]);
            assert!(
                tip.y < root.y - 15.0,
                "blades point up: {root:?} -> {tip:?}"
            );
            assert!(tip.pressure < root.pressure * 0.3, "and lift to a point");
        }
        let plain = Stroke {
            form: None,
            ..stroke
        };
        let pieces = plain.pieces(7);
        assert_eq!(pieces, vec![(plain, 7)]);
    }
}
