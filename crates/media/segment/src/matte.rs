//! How much of each pixel is the subject, and resizing it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "pixel indices are bounded by the matte's size, checked where it is made; the rest is float maths on coverages"
)]

use std::collections::VecDeque;

/// How much of each pixel belongs to the subject, `0..=1`, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Matte {
    pub width: usize,
    pub height: usize,
    pub values: Vec<f32>,
}

impl Matte {
    /// A matte of `values`, clamped to `0..=1`; `None` when the count is not
    /// `width * height` or either is zero.
    pub fn new(width: usize, height: usize, values: Vec<f32>) -> Option<Self> {
        if width == 0 || height == 0 || width.checked_mul(height) != Some(values.len()) {
            return None;
        }
        let values = values
            .into_iter()
            .map(|value| {
                if value.is_finite() {
                    value.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            })
            .collect();
        Some(Self {
            width,
            height,
            values,
        })
    }

    /// The coverage at a pixel, `0` outside.
    pub fn at(&self, x: usize, y: usize) -> f32 {
        if x < self.width && y < self.height {
            self.values[y * self.width + x]
        } else {
            0.0
        }
    }

    /// The same matte at `width` by `height`: averaged over each new
    /// pixel's footprint where it shrinks, interpolated where it grows.
    #[must_use]
    pub fn resized(&self, width: usize, height: usize) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        if (width, height) == (self.width, self.height) {
            return self.clone();
        }
        let across = resample_rows(&self.values, self.width, self.height, width);
        let values = resample_columns(&across, width, self.height, height);
        Self {
            width,
            height,
            values,
        }
    }
}

/// Each row of `plane` (`width` wide) resampled to `to` wide.
fn resample_rows(plane: &[f32], width: usize, height: usize, to: usize) -> Vec<f32> {
    let weights = weights(width, to);
    let mut out = Vec::with_capacity(to * height);
    for row in plane.chunks_exact(width) {
        out.extend(weights.iter().map(|taps| {
            taps.iter()
                .map(|&(at, weight)| row[at] * weight)
                .sum::<f32>()
        }));
    }
    out
}

/// Each column of `plane` (`width` wide, `height` tall) resampled to `to`
/// tall.
fn resample_columns(plane: &[f32], width: usize, height: usize, to: usize) -> Vec<f32> {
    let weights = weights(height, to);
    let mut out = vec![0.0; width * to];
    for (y, taps) in weights.iter().enumerate() {
        let row = &mut out[y * width..(y + 1) * width];
        for &(at, weight) in taps {
            let source = &plane[at * width..(at + 1) * width];
            for (out, value) in row.iter_mut().zip(source) {
                *out += value * weight;
            }
        }
    }
    out
}

/// For each of `to` samples along a side `from` long, the source samples
/// it is made of and their weights: the footprint's area when shrinking,
/// linear interpolation between pixel centres when growing.
fn weights(from: usize, to: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = from as f64 / to as f64;
    let last = from - 1;
    (0..to)
        .map(|index| {
            if scale > 1.0 {
                let (start, end) = (index as f64 * scale, (index + 1) as f64 * scale);
                let mut taps = Vec::new();
                let mut at = start.floor() as usize;
                while (at as f64) < end && at <= last {
                    let covered = (end.min(at as f64 + 1.0) - start.max(at as f64)) / scale;
                    if covered > 0.0 {
                        taps.push((at, covered as f32));
                    }
                    at += 1;
                }
                taps
            } else {
                let centre = ((index as f64 + 0.5) * scale - 0.5).clamp(0.0, last as f64);
                let below = centre.floor() as usize;
                let above = (below + 1).min(last);
                let part = (centre - below as f64) as f32;
                vec![(below, 1.0 - part), (above, part)]
            }
        })
        .collect()
}

/// The part of `matte` that is the one object at pixel `(x, y)`: the
/// connected area of at least half coverage there (or nearest to it,
/// within a twentieth of the matte), with its soft edge. `None` when no
/// object is near.
pub fn object_at(matte: &Matte, x: usize, y: usize) -> Option<Matte> {
    let (width, height) = (matte.width, matte.height);
    let solid = |at: usize| matte.values[at] >= 0.5;
    let reach = (width.max(height) / 20).max(2);
    let seed = nearest_solid(matte, x, y, reach, solid)?;
    let mut inside = vec![false; width * height];
    let mut queue = VecDeque::from([seed]);
    inside[seed] = true;
    while let Some(at) = queue.pop_front() {
        let (cx, cy) = (at % width, at / width);
        for dy in -1_isize..=1 {
            for dx in -1_isize..=1 {
                let (Some(nx), Some(ny)) = (cx.checked_add_signed(dx), cy.checked_add_signed(dy))
                else {
                    continue;
                };
                if nx >= width || ny >= height {
                    continue;
                }
                let next = ny * width + nx;
                if !inside[next] && solid(next) {
                    inside[next] = true;
                    queue.push_back(next);
                }
            }
        }
    }
    // The soft edge around the object belongs to it: keep coverage within
    // a few pixels of what was found.
    let near = grow(&inside, width, height, 3);
    let values = matte
        .values
        .iter()
        .zip(&near)
        .map(|(&value, &near)| if near { value } else { 0.0 })
        .collect();
    Matte::new(width, height, values)
}

/// The solid pixel nearest `(x, y)` within `reach`.
fn nearest_solid(
    matte: &Matte,
    x: usize,
    y: usize,
    reach: usize,
    solid: impl Fn(usize) -> bool,
) -> Option<usize> {
    let (width, height) = (matte.width, matte.height);
    let (x, y) = (x.min(width - 1), y.min(height - 1));
    let mut best: Option<(usize, usize)> = None;
    for ny in y.saturating_sub(reach)..=(y + reach).min(height - 1) {
        for nx in x.saturating_sub(reach)..=(x + reach).min(width - 1) {
            let at = ny * width + nx;
            if !solid(at) {
                continue;
            }
            let distance = nx.abs_diff(x).pow(2) + ny.abs_diff(y).pow(2);
            if best.is_none_or(|(_, best)| distance < best) {
                best = Some((at, distance));
            }
        }
    }
    best.filter(|&(_, distance)| distance <= reach * reach)
        .map(|(at, _)| at)
}

/// `cells` grown by `by` pixels (a square reach).
pub(crate) fn grow(cells: &[bool], width: usize, height: usize, by: usize) -> Vec<bool> {
    let mut across = vec![false; cells.len()];
    for y in 0..height {
        let row = &cells[y * width..(y + 1) * width];
        let mut last: Option<usize> = None;
        let out = &mut across[y * width..(y + 1) * width];
        for (x, &cell) in row.iter().enumerate() {
            if cell {
                last = Some(x);
            }
            out[x] = last.is_some_and(|last| x - last <= by);
        }
        let mut next: Option<usize> = None;
        for x in (0..width).rev() {
            if row[x] {
                next = Some(x);
            }
            out[x] |= next.is_some_and(|next| next - x <= by);
        }
    }
    let mut out = vec![false; cells.len()];
    for x in 0..width {
        let mut last: Option<usize> = None;
        for y in 0..height {
            if across[y * width + x] {
                last = Some(y);
            }
            out[y * width + x] = last.is_some_and(|last| y - last <= by);
        }
        let mut next: Option<usize> = None;
        for y in (0..height).rev() {
            if across[y * width + x] {
                next = Some(y);
            }
            out[y * width + x] |= next.is_some_and(|next| next - y <= by);
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "whole and empty coverage are exact")]
mod tests {
    use super::*;

    #[test]
    fn shrinking_averages_and_growing_interpolates() {
        let matte = Matte::new(4, 1, vec![0.0, 1.0, 1.0, 1.0]).unwrap();
        let half = matte.resized(2, 1);
        assert!((half.values[0] - 0.5).abs() < 1e-6 && (half.values[1] - 1.0).abs() < 1e-6);
        let grown = Matte::new(2, 1, vec![0.0, 1.0]).unwrap().resized(4, 1);
        assert_eq!(grown.values[0], 0.0);
        assert_eq!(grown.values[3], 1.0);
        assert!(grown.values[1] > 0.0 && grown.values[1] < grown.values[2]);
    }

    #[test]
    fn the_object_at_a_point_leaves_the_others_out() {
        // Two blobs on one row: columns 1-2 and 6-7.
        let mut values = vec![0.0; 10 * 3];
        for x in [1, 2, 6, 7] {
            for y in 0..3 {
                values[y * 10 + x] = 1.0;
            }
        }
        let matte = Matte::new(10, 3, values).unwrap();
        let left = object_at(&matte, 1, 1).unwrap();
        assert_eq!(left.at(2, 1), 1.0);
        assert_eq!(left.at(6, 1), 0.0);
        // A click just beside an object finds it.
        let right = object_at(&matte, 8, 1).unwrap();
        assert_eq!(right.at(7, 1), 1.0);
        assert_eq!(right.at(1, 1), 0.0);
    }

    #[test]
    fn nothing_near_finds_no_object() {
        let mut values = vec![0.0; 100 * 100];
        values[0] = 1.0;
        let matte = Matte::new(100, 100, values).unwrap();
        assert!(object_at(&matte, 90, 90).is_none());
    }

    #[test]
    fn a_matte_of_the_wrong_size_is_refused() {
        assert!(Matte::new(3, 3, vec![0.0; 8]).is_none());
        assert!(Matte::new(0, 3, Vec::new()).is_none());
    }
}
