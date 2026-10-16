//! A matte's edge pulled onto the picture's own edges, and the background's
//! colour taken out of the pixels along it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::many_single_char_names,
    clippy::suspicious_operation_groupings,
    reason = "pixel indices are bounded by the picture's size, checked once at the start; the rest is float colour maths (a 3x3 inverse by cofactors among it), named as in the guided filter's paper"
)]

use crate::matte::grow;
use crate::{Error, Matte, Picture};

/// How smooth the refined edge is where the picture's colour is flat: the
/// guided filter's regulariser, on colours `0..=1`.
const SMOOTHNESS: f32 = 1e-3; // a colour variance, not a distance
/// The largest window the filter looks through, in pixels: a wider one
/// blurs fine strands together.
const MOST_WINDOW: usize = 16; // pixels of the picture, not on-screen spacing

/// How far apart, in squared colour, the subject's and the background's
/// colours must be for the pixel's own colour to say how much of it is
/// subject: below this the guided filter alone decides.
const DISTINCT: f32 = 0.02; // a squared colour distance, not spacing
/// How much the colours' own spread (fur's black and white, a busy
/// background) weakens what a pixel's colour says.
const SPREAD: f32 = 4.0; // a ratio of colour variances, not spacing

/// `matte`'s edge refined to `picture` within `radius` pixels of it.
///
/// Along the edge (where the matte is undecided, or within `radius` of
/// where it changes) each pixel's coverage becomes what the colours around
/// it say. A colour guided filter (He, Sun and Tang) fits coverage to
/// colour in each window, so the edge follows the picture's own edges and
/// strands of fur and hair keep their partial coverage. Where the subject
/// and the background nearby are distinct, even colours (an apple on a
/// white table), the pixel's colour is also placed between theirs and can
/// only take coverage away: an edge drawn a few pixels out into a plain
/// background, which a filter cannot see past, comes back to the subject.
/// Away from the edge the matte is kept whole or empty.
///
/// # Errors
/// Returns why the picture or matte cannot be worked on, or that they
/// differ in size.
pub fn refine_edge(picture: &Picture, matte: &Matte, radius: f32) -> Result<Matte, Error> {
    let count = picture.check()?;
    if (matte.width, matte.height) != (picture.width, picture.height) {
        return Err(Error::Size);
    }
    let (width, height) = (picture.width, picture.height);
    let radius = if radius.is_finite() {
        radius.clamp(1.0, 256.0)
    } else {
        1.0
    };
    let reach = radius.round() as usize;
    let window = reach.clamp(2, MOST_WINDOW);
    let hard: Vec<bool> = matte.values.iter().map(|&value| value >= 0.5).collect();
    let edge = edge_band(&hard, &matte.values, width, height, reach);
    let colours = picture.colours();
    let guided = guided_filter(&colours, &matte.values, width, height, window);
    let placed = colour_line(&colours, &hard, (width, height), reach);
    let values = (0..count)
        .map(|at| {
            if edge[at] {
                let (between, sure) = placed[at];
                let guided = guided[at];
                let coverage = guided.min(sure * between + (1.0 - sure) * guided);
                // Stretch so a strand's faint tail goes to nothing and its
                // core to whole, keeping the soft part between.
                ((coverage - 0.1) / 0.8).clamp(0.0, 1.0)
            } else if hard[at] {
                1.0
            } else {
                0.0
            }
        })
        .collect();
    Matte::new(width, height, values).ok_or(Error::Size)
}

/// For each pixel, where its colour lies between the subject's and the
/// background's nearby (`0` the background's, `1` the subject's), and how
/// sure that is (`0..=1`). Their colours are the means of the pixels
/// surely in and surely out (further than `reach` from the edge) around it.
fn colour_line(
    colours: &[[f32; 3]],
    hard: &[bool],
    (width, height): (usize, usize),
    reach: usize,
) -> Vec<(f32, f32)> {
    let outside: Vec<bool> = hard.iter().map(|&inside| !inside).collect();
    let near_out = grow(&outside, width, height, reach);
    let near_in = grow(hard, width, height, reach);
    let sure_in: Vec<f32> = hard
        .iter()
        .zip(&near_out)
        .map(|(&inside, &near)| f32::from(u8::from(inside && !near)))
        .collect();
    let sure_out: Vec<f32> = outside
        .iter()
        .zip(&near_in)
        .map(|(&outside, &near)| f32::from(u8::from(outside && !near)))
        .collect();
    let around = (reach * 2).max(8);
    let mean = |plane: Vec<f32>| box_mean(&plane, width, height, around);
    let weight_in = mean(sure_in.clone());
    let weight_out = mean(sure_out.clone());
    // Per channel: the weighted mean colour and mean square, in and out.
    let mut subject = vec![[0.0_f32; 3]; colours.len()];
    let mut background = vec![[0.0_f32; 3]; colours.len()];
    let mut spread = vec![0.0_f32; colours.len()];
    for c in 0..3 {
        for (weights, total, into) in [
            (&sure_in, &weight_in, &mut subject),
            (&sure_out, &weight_out, &mut background),
        ] {
            let sum = mean(
                colours
                    .iter()
                    .zip(weights)
                    .map(|(colour, w)| colour[c] * w)
                    .collect(),
            );
            let squares = mean(
                colours
                    .iter()
                    .zip(weights)
                    .map(|(colour, w)| colour[c] * colour[c] * w)
                    .collect(),
            );
            for at in 0..colours.len() {
                let m = sum[at] / (total[at] + 1e-6);
                into[at][c] = m;
                spread[at] += (squares[at] / (total[at] + 1e-6) - m * m).max(0.0);
            }
        }
    }
    (0..colours.len())
        .map(|at| {
            if weight_in[at] < 1e-3 || weight_out[at] < 1e-3 {
                return (0.0, 0.0);
            }
            let (f, b, i) = (subject[at], background[at], colours[at]);
            let d = [f[0] - b[0], f[1] - b[1], f[2] - b[2]];
            let distance = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
            let along = (i[0] - b[0]) * d[0] + (i[1] - b[1]) * d[1] + (i[2] - b[2]) * d[2];
            let between = (along / (distance + 1e-6)).clamp(0.0, 1.0);
            let sure = distance / (distance + DISTINCT + SPREAD * spread[at]);
            (between, sure)
        })
        .collect()
}

/// The pixels near the matte's edge: where it changes within `reach`, or
/// is undecided.
fn edge_band(hard: &[bool], soft: &[f32], width: usize, height: usize, reach: usize) -> Vec<bool> {
    let inside = grow(hard, width, height, reach);
    let outside: Vec<bool> = hard.iter().map(|&inside| !inside).collect();
    let outside = grow(&outside, width, height, reach);
    inside
        .iter()
        .zip(&outside)
        .zip(soft)
        .map(|((&inside, &outside), &soft)| (inside && outside) || (0.02..0.98).contains(&soft))
        .collect()
}

/// The colour guided filter of `p` by `guide` with a `(2r+1)²` window.
fn guided_filter(guide: &[[f32; 3]], p: &[f32], width: usize, height: usize, r: usize) -> Vec<f32> {
    let count = width * height;
    let channel = |c: usize| guide.iter().map(|colour| colour[c]).collect::<Vec<f32>>();
    let i = [channel(0), channel(1), channel(2)];
    let mean = |plane: &[f32]| box_mean(plane, width, height, r);
    let mean_i = [mean(&i[0]), mean(&i[1]), mean(&i[2])];
    let mean_p = mean(p);
    let product = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(a, b)| a * b).collect::<Vec<f32>>();
    let cov_ip: Vec<Vec<f32>> = (0..3)
        .map(|c| {
            let m = mean(&product(&i[c], p));
            m.iter()
                .zip(&mean_i[c])
                .zip(&mean_p)
                .map(|((m, mi), mp)| m - mi * mp)
                .collect()
        })
        .collect();
    // The guide's covariance in each window: rr, rg, rb, gg, gb, bb.
    let pairs = [(0, 0), (0, 1), (0, 2), (1, 1), (1, 2), (2, 2)];
    let var: Vec<Vec<f32>> = pairs
        .iter()
        .map(|&(a, b)| {
            let m = mean(&product(&i[a], &i[b]));
            m.iter()
                .zip(&mean_i[a])
                .zip(&mean_i[b])
                .map(|((m, ma), mb)| m - ma * mb)
                .collect()
        })
        .collect();
    let mut a = [vec![0.0; count], vec![0.0; count], vec![0.0; count]];
    let mut b = vec![0.0; count];
    for at in 0..count {
        let e = SMOOTHNESS;
        let (rr, rg, rb) = (var[0][at] + e, var[1][at], var[2][at]);
        let (gg, gb, bb) = (var[3][at] + e, var[4][at], var[5][at] + e);
        // The inverse of the symmetric 3x3 by its cofactors.
        let c = [
            gg * bb - gb * gb,
            gb * rb - rg * bb,
            rg * gb - gg * rb,
            rr * bb - rb * rb,
            rg * rb - rr * gb,
            rr * gg - rg * rg,
        ];
        let det = rr * c[0] + rg * c[1] + rb * c[2];
        let (x, y, z) = (cov_ip[0][at], cov_ip[1][at], cov_ip[2][at]);
        let (ar, ag, ab) = if det.abs() > 1e-12 {
            (
                (c[0] * x + c[1] * y + c[2] * z) / det,
                (c[1] * x + c[3] * y + c[4] * z) / det,
                (c[2] * x + c[4] * y + c[5] * z) / det,
            )
        } else {
            (0.0, 0.0, 0.0)
        };
        a[0][at] = ar;
        a[1][at] = ag;
        a[2][at] = ab;
        b[at] = mean_p[at] - ar * mean_i[0][at] - ag * mean_i[1][at] - ab * mean_i[2][at];
    }
    let mean_a = [mean(&a[0]), mean(&a[1]), mean(&a[2])];
    let mean_b = mean(&b);
    (0..count)
        .map(|at| {
            mean_a[0][at] * i[0][at]
                + mean_a[1][at] * i[1][at]
                + mean_a[2][at] * i[2][at]
                + mean_b[at]
        })
        .collect()
}

/// The mean of each pixel's `(2r+1)²` window, over the part of it inside
/// the plane.
fn box_mean(plane: &[f32], width: usize, height: usize, r: usize) -> Vec<f32> {
    let across = box_line(plane, width, height, r, true);
    box_line(&across, width, height, r, false)
}

/// The mean along rows (or columns) of a `2r+1` window, by running sums.
fn box_line(plane: &[f32], width: usize, height: usize, r: usize, rows: bool) -> Vec<f32> {
    let (lines, length) = if rows {
        (height, width)
    } else {
        (width, height)
    };
    let index = |line: usize, along: usize| {
        if rows {
            line * width + along
        } else {
            along * width + line
        }
    };
    let mut out = vec![0.0; plane.len()];
    let mut sums = vec![0.0_f64; length + 1];
    for line in 0..lines {
        for along in 0..length {
            sums[along + 1] = sums[along] + f64::from(plane[index(line, along)]);
        }
        for along in 0..length {
            let start = along.saturating_sub(r);
            let end = (along + r + 1).min(length);
            out[index(line, along)] = ((sums[end] - sums[start]) / (end - start) as f64) as f32;
        }
    }
    out
}

/// `picture` with the background's colour taken out of the pixels along
/// `alpha`'s edge, so laid over anything else the subject brings no halo
/// of what it stood in front of.
///
/// Each partly covered pixel is a mix of the subject's colour and the
/// background's. Their colours nearby (the subject's from the covered
/// pixels around, the background's from the uncovered ones) are estimated
/// by blurs weighted by coverage, then the pixel's own colour is solved for
/// the subject's part (Forte and Pitié's blur fusion, a wide pass then a
/// close one). Pixels wholly in or out are kept as they are; alpha is
/// never changed.
///
/// # Errors
/// Returns why the picture or matte cannot be worked on, or that they
/// differ in size.
pub fn defringe(picture: &Picture, alpha: &Matte) -> Result<Vec<u8>, Error> {
    let count = picture.check()?;
    if (alpha.width, alpha.height) != (picture.width, picture.height) {
        return Err(Error::Size);
    }
    let (width, height) = (picture.width, picture.height);
    let colours = picture.colours();
    let a = &alpha.values;
    let wide = (width.max(height) / 24).clamp(8, 90);
    let (first, background) = fuse(&colours, a, &colours, &colours, width, height, wide);
    let (subject, _) = fuse(&colours, a, &first, &background, width, height, 3);
    // Only the edge and a pixel around it change.
    let partial: Vec<bool> = a.iter().map(|&a| a > 0.004 && a < 0.996).collect();
    let edge = grow(&partial, width, height, 1);
    let mut out = picture.rgba.to_vec();
    for at in 0..count {
        if !edge[at] || a[at] <= 0.004 {
            continue;
        }
        for c in 0..3 {
            out[at * 4 + c] = (subject[at][c] * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    Ok(out)
}

/// One pass of blur fusion: the subject's colour estimated from `subject`
/// and the background's from `background`, both blurred over `r` and
/// weighted by coverage, then the pixel's colour solved for the subject.
fn fuse(
    image: &[[f32; 3]],
    alpha: &[f32],
    subject: &[[f32; 3]],
    background: &[[f32; 3]],
    width: usize,
    height: usize,
    r: usize,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let mean = |plane: Vec<f32>| box_mean(&plane, width, height, r);
    let blurred_alpha = mean(alpha.to_vec());
    let mut f = vec![[0.0; 3]; image.len()];
    let mut b = vec![[0.0; 3]; image.len()];
    for c in 0..3 {
        let fa = mean(subject.iter().zip(alpha).map(|(s, a)| s[c] * a).collect());
        let bb = mean(
            background
                .iter()
                .zip(alpha)
                .map(|(s, a)| s[c] * (1.0 - a))
                .collect(),
        );
        for at in 0..image.len() {
            let blurred_f = fa[at] / (blurred_alpha[at] + 1e-5);
            let blurred_b = bb[at] / ((1.0 - blurred_alpha[at]) + 1e-5);
            let a = alpha[at];
            f[at][c] = (blurred_f + a * (image[at][c] - a * blurred_f - (1.0 - a) * blurred_b))
                .clamp(0.0, 1.0);
            b[at][c] = blurred_b;
        }
    }
    (f, b)
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "whole and empty coverage are exact")]
mod tests {
    use super::*;

    /// A picture `width` wide: red left of `edge`, white from it on.
    fn red_on_white(width: usize, height: usize, edge: usize) -> Vec<u8> {
        (0..width * height)
            .flat_map(|at| {
                if at % width < edge {
                    [200, 20, 20, 255]
                } else {
                    [255, 255, 255, 255]
                }
            })
            .collect()
    }

    #[test]
    fn a_soft_edge_guessed_off_the_picture_snaps_onto_it() {
        let (width, height) = (40, 8);
        let rgba = red_on_white(width, height, 20);
        let picture = Picture {
            rgba: &rgba,
            width,
            height,
        };
        // The guess fades out over columns 18..26: too soft and too far out.
        let values = (0..width * height)
            .map(|at| ((26.0 - (at % width) as f32) / 8.0).clamp(0.0, 1.0))
            .collect();
        let matte = Matte::new(width, height, values).unwrap();
        let refined = refine_edge(&picture, &matte, 6.0).unwrap();
        assert!(refined.at(18, 4) > 0.95, "inside: {}", refined.at(18, 4));
        // Out in the background the guess's tail is mostly gone.
        assert!(
            refined.at(21, 4) < 0.4,
            "just outside: {}",
            refined.at(21, 4)
        );
        assert!(refined.at(22, 4) < 0.2, "outside: {}", refined.at(22, 4));
        assert_eq!(refined.at(2, 4), 1.0);
        assert_eq!(refined.at(38, 4), 0.0);
    }

    #[test]
    fn the_background_comes_out_of_a_partly_covered_pixel() {
        let (width, height) = (40, 8);
        let mut rgba = red_on_white(width, height, 20);
        // Column 20 is half red, half the white behind it.
        let mut values = vec![0.0; width * height];
        for y in 0..height {
            for x in 0..20 {
                values[y * width + x] = 1.0;
            }
            values[y * width + 20] = 0.5;
            rgba[(y * width + 20) * 4..(y * width + 20) * 4 + 3].copy_from_slice(&[228, 138, 138]);
        }
        let picture = Picture {
            rgba: &rgba,
            width,
            height,
        };
        let alpha = Matte::new(width, height, values).unwrap();
        let clean = defringe(&picture, &alpha).unwrap();
        let pixel = &clean[(4 * width + 20) * 4..(4 * width + 20) * 4 + 3];
        assert!(
            pixel[0] > 180 && pixel[1] < 60 && pixel[2] < 60,
            "{pixel:?}"
        );
        // Whole and empty pixels are left alone.
        assert_eq!(
            &clean[(4 * width + 5) * 4..(4 * width + 5) * 4 + 4],
            &[200, 20, 20, 255]
        );
        assert_eq!(
            &clean[(4 * width + 35) * 4..(4 * width + 35) * 4 + 4],
            &[255, 255, 255, 255]
        );
    }

    #[test]
    fn a_matte_of_another_size_is_refused() {
        let rgba = red_on_white(4, 4, 2);
        let picture = Picture {
            rgba: &rgba,
            width: 4,
            height: 4,
        };
        let matte = Matte::new(2, 2, vec![0.0; 4]).unwrap();
        assert_eq!(refine_edge(&picture, &matte, 2.0), Err(Error::Size));
        assert_eq!(defringe(&picture, &matte), Err(Error::Size));
    }
}
