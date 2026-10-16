//! Colour and tone adjustments, as one deterministic pipeline every view of
//! the artwork shares: the canvas, a look, an export and a thumbnail grade
//! a picture's pixels and a shape's colours with the same arithmetic.
//!
//! The per-pixel steps are prepared once into a [`Grade`]: a tone table per
//! channel (white balance, exposure, contrast, curve), the tone
//! shift of highlights and shadows, one colour matrix (hue, saturation,
//! grayscale), vibrance and inversion, run over a picture's pixels in bands
//! on the toolkit's compute threads. Blur and sharpen work on pixels
//! only; a vignette is drawn over what it darkens, so it is the same on
//! pictures, paint and vector art.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "pixel arithmetic in bounded floats and bytes; every index is from an iterator"
)]

use crate::model::Adjust;

/// Samples of a tone table across `0..=1`.
const TONE_SAMPLES: usize = 1024;

/// Rec. 709 luma weights.
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

impl Adjust {
    /// Whether it changes nothing.
    pub fn is_identity(&self) -> bool {
        self.clone().normalized().is_none()
    }

    /// The same adjustments with every step that changes nothing left out;
    /// `None` when none is left.
    #[must_use]
    pub fn normalized(self) -> Option<Self> {
        let zero = |value: Option<f64>| value.filter(|value| value.abs() > 1e-9);
        let adjust = Self {
            exposure: zero(self.exposure),
            contrast: zero(self.contrast),
            highlights: zero(self.highlights),
            shadows: zero(self.shadows),
            saturation: zero(self.saturation),
            vibrance: zero(self.vibrance),
            hue: zero(self.hue),
            temperature: zero(self.temperature),
            tint: zero(self.tint),
            curve: self.curve.filter(|curve| {
                !curve
                    .iter()
                    .all(|[input, output]| (input - output).abs() <= 1e-9)
            }),
            grayscale: self.grayscale.filter(|on| *on),
            invert: self.invert.filter(|on| *on),
            blur: zero(self.blur),
            sharpen: zero(self.sharpen),
            vignette: zero(self.vignette),
        };
        (adjust != Self::default()).then_some(adjust)
    }

    /// `edit` over these adjustments: each step it names replaces the one
    /// here; an edit naming none (`{}`) clears them all.
    #[must_use]
    pub fn merged(&self, edit: &Self) -> Option<Self> {
        if *edit == Self::default() {
            return None;
        }
        let base = self.clone();
        let pick = |new: Option<f64>, old: Option<f64>| new.or(old);
        Self {
            exposure: pick(edit.exposure, base.exposure),
            contrast: pick(edit.contrast, base.contrast),
            highlights: pick(edit.highlights, base.highlights),
            shadows: pick(edit.shadows, base.shadows),
            saturation: pick(edit.saturation, base.saturation),
            vibrance: pick(edit.vibrance, base.vibrance),
            hue: pick(edit.hue, base.hue),
            temperature: pick(edit.temperature, base.temperature),
            tint: pick(edit.tint, base.tint),
            curve: edit.curve.clone().or(base.curve),
            grayscale: edit.grayscale.or(base.grayscale),
            invert: edit.invert.or(base.invert),
            blur: pick(edit.blur, base.blur),
            sharpen: pick(edit.sharpen, base.sharpen),
            vignette: pick(edit.vignette, base.vignette),
        }
        .normalized()
    }

    /// Why these adjustments cannot apply, naming the step and its range.
    pub fn problem(&self) -> Option<String> {
        let ranges = [
            (self.exposure, "exposure", -5.0, 5.0),
            (self.contrast, "contrast", -1.0, 1.0),
            (self.highlights, "highlights", -1.0, 1.0),
            (self.shadows, "shadows", -1.0, 1.0),
            (self.saturation, "saturation", -1.0, 1.0),
            (self.vibrance, "vibrance", -1.0, 1.0),
            (self.hue, "hue", -180.0, 180.0),
            (self.temperature, "temperature", -1.0, 1.0),
            (self.tint, "tint", -1.0, 1.0),
            (self.blur, "blur", 0.0, 200.0),
            (self.sharpen, "sharpen", 0.0, 1.0),
            (self.vignette, "vignette", -1.0, 1.0),
        ];
        for (value, name, low, high) in ranges {
            if let Some(value) = value
                && !(value.is_finite() && (low..=high).contains(&value))
            {
                return Some(format!("{name} is {low} to {high}, got {value}"));
            }
        }
        if let Some(curve) = &self.curve
            && (curve.len() > 16
                || curve
                    .iter()
                    .flatten()
                    .any(|value| !(0.0..=1.0).contains(value)))
        {
            return Some("curve is up to 16 points [in, out], each 0 to 1".into());
        }
        None
    }

    /// The steps that act on each pixel alone, prepared.
    pub fn grade(&self) -> Option<Grade> {
        Grade::new(self)
    }
}

/// The per-pixel steps of an [`Adjust`], prepared to run over many pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Grade {
    /// Per channel: the tone each input level becomes.
    tone: Option<[Vec<f32>; 3]>,
    shadows: f32,
    highlights: f32,
    matrix: Option<[[f32; 3]; 3]>,
    vibrance: f32,
    invert: bool,
}

impl Grade {
    fn new(adjust: &Adjust) -> Option<Self> {
        let narrow = |value: Option<f64>| value.unwrap_or(0.0) as f32;
        let toned = adjust.exposure.is_some()
            || adjust.temperature.is_some()
            || adjust.tint.is_some()
            || adjust.contrast.is_some()
            || adjust.curve.as_ref().is_some_and(|curve| !curve.is_empty());
        let tone = toned.then(|| {
            let temperature = narrow(adjust.temperature);
            let tint = narrow(adjust.tint);
            // White balance in linear light: warmer lifts red and lowers
            // blue; magenta lowers green.
            let gains = [
                (1.0 + 0.3 * temperature) * (1.0 + 0.1 * tint),
                1.0 - 0.2 * tint,
                (1.0 - 0.3 * temperature) * (1.0 + 0.1 * tint),
            ];
            let exposure = narrow(adjust.exposure).exp2();
            let curve = adjust.curve.as_deref().map(Curve::new);
            gains.map(|gain| {
                (0..TONE_SAMPLES)
                    .map(|index| {
                        let input = index as f32 / (TONE_SAMPLES - 1) as f32;
                        let mut value = to_srgb(to_linear(input) * gain * exposure);
                        value = contrast(value, narrow(adjust.contrast));
                        if let Some(curve) = &curve {
                            value = curve.at(value);
                        }
                        value.clamp(0.0, 1.0)
                    })
                    .collect()
            })
        });
        let mut matrix: Option<[[f32; 3]; 3]> = adjust.hue.map(|hue| hue_matrix(hue as f32));
        if let Some(saturation) = adjust.saturation {
            let saturate = saturate_matrix(1.0 + saturation as f32);
            matrix = Some(matrix.map_or(saturate, |before| multiply(&saturate, &before)));
        }
        if adjust.grayscale == Some(true) {
            let grey = saturate_matrix(0.0);
            matrix = Some(matrix.map_or(grey, |before| multiply(&grey, &before)));
        }
        let grade = Self {
            tone,
            shadows: narrow(adjust.shadows),
            highlights: narrow(adjust.highlights),
            matrix,
            vibrance: narrow(adjust.vibrance),
            invert: adjust.invert == Some(true),
        };
        let identity = grade.tone.is_none()
            && grade.shadows == 0.0
            && grade.highlights == 0.0
            && grade.matrix.is_none()
            && grade.vibrance == 0.0
            && !grade.invert;
        (!identity).then_some(grade)
    }

    /// One colour, each channel `0..=1`, graded.
    #[allow(
        clippy::many_single_char_names,
        reason = "r, g, b are the channels' names"
    )]
    pub fn rgb(&self, [mut r, mut g, mut b]: [f32; 3]) -> [f32; 3] {
        if let Some([red, green, blue]) = &self.tone {
            r = lookup(red, r);
            g = lookup(green, g);
            b = lookup(blue, b);
        }
        if self.shadows != 0.0 || self.highlights != 0.0 {
            let luma = LUMA[0] * r + LUMA[1] * g + LUMA[2] * b;
            // Each moves the tones around its third of the range, not black
            // or white themselves.
            let shift = 0.25
                * (self.shadows * 6.75 * luma * (1.0 - luma) * (1.0 - luma)
                    + self.highlights * 6.75 * luma * luma * (1.0 - luma));
            if luma > 1e-4 {
                let gain = ((luma + shift) / luma).max(0.0);
                [r, g, b] = [r * gain, g * gain, b * gain];
            } else {
                [r, g, b] = [r + shift, g + shift, b + shift];
            }
        }
        if let Some(matrix) = &self.matrix {
            let [x, y, z] = matrix.map(|row| row[0] * r + row[1] * g + row[2] * b);
            [r, g, b] = [x, y, z];
        }
        if self.vibrance != 0.0 {
            let high = r.max(g).max(b);
            let low = r.min(g).min(b);
            let amount = if self.vibrance > 0.0 {
                self.vibrance * (1.0 - (high - low).clamp(0.0, 1.0))
            } else {
                self.vibrance
            };
            let luma = LUMA[0] * r + LUMA[1] * g + LUMA[2] * b;
            [r, g, b] = [r, g, b].map(|channel| luma + (channel - luma) * (1.0 + amount));
        }
        if self.invert {
            [r, g, b] = [1.0 - r, 1.0 - g, 1.0 - b];
        }
        [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
    }

    /// A colour as `#rrggbbaa`, graded; its alpha kept.
    pub fn colour(&self, [r, g, b, a]: [u8; 4]) -> [u8; 4] {
        let [r, g, b] = self.rgb([r, g, b].map(|channel| f32::from(channel) / 255.0));
        [byte(r), byte(g), byte(b), a]
    }
}

fn byte(value: f32) -> u8 {
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

fn lookup(table: &[f32], value: f32) -> f32 {
    let at = value.clamp(0.0, 1.0) * (TONE_SAMPLES - 1) as f32;
    let index = at.floor() as usize;
    let low = table.get(index).copied().unwrap_or(value);
    let high = table.get(index + 1).copied().unwrap_or(low);
    low + (high - low) * (at - index as f32)
}

fn to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(value: f32) -> f32 {
    let value = value.max(0.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        (1.055 * value.powf(1.0 / 2.4) - 0.055).min(1.0)
    }
}

/// Contrast about the middle: more pulls tones apart along an S that
/// keeps black and white; less draws them toward grey.
fn contrast(value: f32, amount: f32) -> f32 {
    if amount < 0.0 {
        return 0.5 + (value - 0.5) * (1.0 + amount);
    }
    if amount == 0.0 {
        return value;
    }
    let steep = 10.0 * amount;
    let sigmoid = |x: f32| 1.0 / (1.0 + (-steep * (x - 0.5)).exp());
    let (low, high) = (sigmoid(0.0), sigmoid(1.0));
    (sigmoid(value) - low) / (high - low)
}

/// A tone curve through points, monotone between them (Fritsch-Carlson),
/// flat past the first and last.
struct Curve {
    points: Vec<(f32, f32)>,
    slopes: Vec<f32>,
}

impl Curve {
    fn new(points: &[[f64; 2]]) -> Self {
        let mut points: Vec<(f32, f32)> =
            points.iter().map(|[x, y]| (*x as f32, *y as f32)).collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        points.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-4);
        let secants: Vec<f32> = points
            .windows(2)
            .map(|pair| match pair {
                [(x0, y0), (x1, y1)] => (y1 - y0) / (x1 - x0).max(1e-4),
                _ => 0.0,
            })
            .collect();
        let mut slopes: Vec<f32> = (0..points.len())
            .map(|index| {
                let before = index.checked_sub(1).and_then(|at| secants.get(at));
                let after = secants.get(index);
                match (before, after) {
                    (Some(a), Some(b)) if a * b > 0.0 => (a + b) / 2.0,
                    (Some(a), None) => *a,
                    (None, Some(b)) => *b,
                    // A peak or valley between two sides, or one point.
                    _ => 0.0,
                }
            })
            .collect();
        for (index, secant) in secants.iter().enumerate() {
            if secant.abs() < 1e-6 {
                for at in [index, index + 1] {
                    if let Some(slope) = slopes.get_mut(at) {
                        *slope = 0.0;
                    }
                }
                continue;
            }
            let a = slopes.get(index).copied().unwrap_or(0.0) / secant;
            let b = slopes.get(index + 1).copied().unwrap_or(0.0) / secant;
            let length = a.hypot(b);
            if length > 3.0 {
                let shrink = 3.0 / length;
                if let Some(slope) = slopes.get_mut(index) {
                    *slope = shrink * a * secant;
                }
                if let Some(slope) = slopes.get_mut(index + 1) {
                    *slope = shrink * b * secant;
                }
            }
        }
        Self { points, slopes }
    }

    fn at(&self, x: f32) -> f32 {
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else {
            return x;
        };
        if x <= first.0 {
            return first.1;
        }
        if x >= last.0 {
            return last.1;
        }
        let index = self
            .points
            .partition_point(|point| point.0 <= x)
            .saturating_sub(1);
        let (Some((x0, y0)), Some((x1, y1))) = (self.points.get(index), self.points.get(index + 1))
        else {
            return x;
        };
        let span = (x1 - x0).max(1e-6);
        let t = (x - x0) / span;
        let (m0, m1) = (
            self.slopes.get(index).copied().unwrap_or(0.0) * span,
            self.slopes.get(index + 1).copied().unwrap_or(0.0) * span,
        );
        let (t2, t3) = (t * t, t * t * t);
        (2.0 * t3 - 3.0 * t2 + 1.0) * y0
            + (t3 - 2.0 * t2 + t) * m0
            + (-2.0 * t3 + 3.0 * t2) * y1
            + (t3 - t2) * m1
    }
}

/// Turning hue by `degrees` about the grey axis, as CSS's `hue-rotate`.
fn hue_matrix(degrees: f32) -> [[f32; 3]; 3] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [
        [
            0.213 + cos * 0.787 - sin * 0.213,
            0.715 - cos * 0.715 - sin * 0.715,
            0.072 - cos * 0.072 + sin * 0.928,
        ],
        [
            0.213 - cos * 0.213 + sin * 0.143,
            0.715 + cos * 0.285 + sin * 0.140,
            0.072 - cos * 0.072 - sin * 0.283,
        ],
        [
            0.213 - cos * 0.213 - sin * 0.787,
            0.715 - cos * 0.715 + sin * 0.715,
            0.072 + cos * 0.928 + sin * 0.072,
        ],
    ]
}

/// Saturation times `amount` (0 is grey), as CSS's `saturate`.
fn saturate_matrix(amount: f32) -> [[f32; 3]; 3] {
    let s = amount;
    [
        [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
    ]
}

/// `a` after `b`.
fn multiply(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let column = |index: usize| b.map(|row| row.get(index).copied().unwrap_or(0.0));
    a.map(|row| {
        [0, 1, 2].map(|index| {
            row.iter()
                .zip(column(index))
                .map(|(x, y)| x * y)
                .sum::<f32>()
        })
    })
}

/// Straight RGBA pixels graded by each of `grades` in turn; alpha kept.
pub fn grade_pixels(pixels: &mut [u8], grades: &[Grade]) {
    if grades.is_empty() {
        return;
    }
    bands(pixels, 4, |band| {
        for pixel in band.chunks_exact_mut(4) {
            let mut rgb = [0.0_f32; 3];
            for (value, channel) in rgb.iter_mut().zip(pixel.iter()) {
                *value = f32::from(*channel) / 255.0;
            }
            for grade in grades {
                rgb = grade.rgb(rgb);
            }
            for (channel, value) in pixel.iter_mut().zip(rgb) {
                *channel = byte(value);
            }
        }
    });
}

/// Run `work` over `pixels` split into bands of whole units of `stride`
/// bytes, on the toolkit's compute threads (the caller's alone where there
/// are none), returning when every band is done. The bands do not depend on
/// how many threads there are, so the result is the same everywhere.
fn bands(pixels: &mut [u8], stride: usize, work: impl Fn(&mut [u8]) + Sync) {
    let units = pixels.len() / stride.max(1);
    let per = units.div_ceil(BANDS).max(BAND_UNITS) * stride.max(1);
    let chunks: Vec<&mut [u8]> = pixels.chunks_mut(per).collect();
    each(chunks, &work);
}

/// How many bands a picture is split into for its threads.
const BANDS: usize = 16;
/// The fewest units a band holds: a smaller one costs more to hand over
/// than to do.
const BAND_UNITS: usize = 4096;

/// `work` on each of `parts`, on the toolkit's compute threads, returning
/// when all are done.
fn each<T: Send>(parts: Vec<T>, work: &(impl Fn(T) + Sync)) {
    if parts.len() <= 1 {
        parts.into_iter().for_each(work);
        return;
    }
    ui::rt::cpu_pool().scope(|scope| {
        for part in parts {
            scope.spawn(move |_| work(part));
        }
    });
}

/// An RGBA picture blurred like a gaussian `radius` pixels wide (three box
/// passes each way), its edges held.
pub fn blur(pixels: &mut [u8], width: usize, height: usize, radius: f32) {
    if radius < 0.5 || width == 0 || height == 0 {
        return;
    }
    // Box widths whose three passes approximate a gaussian of sigma
    // radius / 2.
    let sigma = radius / 2.0;
    let ideal = (12.0 * sigma * sigma / 3.0 + 1.0).sqrt();
    let mut lower = ideal.floor() as usize;
    if lower.is_multiple_of(2) {
        lower = lower.saturating_sub(1);
    }
    let lower = lower.max(1);
    let upper = lower + 2;
    let wide = (12.0 * sigma * sigma - 3.0 * (lower * lower) as f32 - 12.0 * lower as f32 - 9.0)
        / (-4.0 * lower as f32 - 4.0);
    let wide = wide.round().max(0.0) as usize;
    for pass in 0..3 {
        let size = if pass < wide { lower } else { upper };
        let reach = size / 2;
        if reach == 0 {
            continue;
        }
        box_rows(pixels, width, reach);
        box_columns(pixels, width, height, reach);
    }
}

/// `2^32 / span`, rounded up: dividing by `span` as a multiplication.
fn reciprocal(span: u32) -> u64 {
    (1_u64 << 32).div_ceil(u64::from(span.max(1)))
}

/// The rounded mean of `span` bytes summing to `sum`.
fn mean(sum: u32, span: u32, scale: u64) -> u8 {
    ((u64::from(sum + span / 2) * scale) >> 32).min(255) as u8
}

/// Each row averaged over `reach` pixels either side.
fn box_rows(pixels: &mut [u8], width: usize, reach: usize) {
    let row_bytes = width * 4;
    let last = width.saturating_sub(1);
    let span = u32::try_from(2 * reach + 1).unwrap_or(u32::MAX);
    let scale = reciprocal(span);
    bands(pixels, row_bytes, |band| {
        let mut source = vec![0_u8; row_bytes];
        for row in band.chunks_exact_mut(row_bytes) {
            source.copy_from_slice(row);
            let at = |x: usize| -> [u32; 4] {
                let start = x.min(last) * 4;
                let pixel = source.get(start..start + 4).unwrap_or(&[0; 4]);
                let mut channels = [0_u32; 4];
                for (channel, value) in channels.iter_mut().zip(pixel) {
                    *channel = u32::from(*value);
                }
                channels
            };
            let mut sums = [0_u32; 4];
            for step in 0..=2 * reach {
                for (sum, value) in sums.iter_mut().zip(at(step.saturating_sub(reach))) {
                    *sum += value;
                }
            }
            for (x, out) in row.chunks_exact_mut(4).enumerate() {
                let (add, sub) = (at(x + reach + 1), at(x.saturating_sub(reach)));
                for (((channel, sum), add), sub) in
                    out.iter_mut().zip(sums.iter_mut()).zip(add).zip(sub)
                {
                    *channel = mean(*sum, span, scale);
                    *sum = *sum + add - sub;
                }
            }
        }
    });
}

/// Each column averaged over `reach` pixels above and below.
fn box_columns(pixels: &mut [u8], width: usize, height: usize, reach: usize) {
    let row_bytes = width * 4;
    let source = pixels.to_vec();
    let span = u32::try_from(2 * reach + 1).unwrap_or(u32::MAX);
    let scale = reciprocal(span);
    let last = height.saturating_sub(1);
    let row = |y: usize| -> &[u8] {
        let y = y.min(last);
        source
            .get(y * row_bytes..(y + 1) * row_bytes)
            .unwrap_or(&[])
    };
    let rows_per = height.div_ceil(BANDS).max(1);
    let chunks: Vec<&mut [u8]> = pixels.chunks_mut(rows_per * row_bytes).collect();
    let work = |(band, chunk): (usize, &mut [u8])| {
        let first = band * rows_per;
        let mut sums = vec![0_u32; row_bytes];
        for step in 0..=2 * reach {
            for (sum, value) in sums
                .iter_mut()
                .zip(row((first + step).saturating_sub(reach)))
            {
                *sum += u32::from(*value);
            }
        }
        for (offset, out) in chunk.chunks_exact_mut(row_bytes).enumerate() {
            let y = first + offset;
            for (pixel, sum) in out.iter_mut().zip(&sums) {
                *pixel = mean(*sum, span, scale);
            }
            for ((sum, add), sub) in sums
                .iter_mut()
                .zip(row(y + reach + 1))
                .zip(row(y.saturating_sub(reach)))
            {
                *sum = *sum + u32::from(*add) - u32::from(*sub);
            }
        }
    };
    each(chunks.into_iter().enumerate().collect(), &work);
}

/// An RGBA picture sharpened by `amount` (0-1): its difference from a
/// `radius`-pixel blur of itself added back.
pub fn sharpen(pixels: &mut [u8], width: usize, height: usize, amount: f32, radius: f32) {
    if amount <= 0.0 || width == 0 || height == 0 {
        return;
    }
    let mut soft = pixels.to_vec();
    blur(&mut soft, width, height, radius.max(1.0));
    let gain = amount * 1.5;
    for (pixel, soft) in pixels.chunks_exact_mut(4).zip(soft.chunks_exact(4)) {
        for (channel, blurred) in pixel.iter_mut().zip(soft).take(3) {
            let value = f32::from(*channel);
            *channel = (value + gain * (value - f32::from(*blurred)))
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
}

/// How far a vignette of `amount` darkens (or, below zero, lightens) at
/// `distance` from the middle, 1 at the middle of an edge: the alpha of
/// black (white) laid over it.
pub fn vignette_alpha(amount: f64, distance: f64) -> f64 {
    let t = ((distance - 0.35) / (1.45 - 0.35)).clamp(0.0, 1.0);
    amount.abs().min(1.0) * t * t * (3.0 - 2.0 * t)
}

/// The vignette's reach, in the units where an edge's middle is 1: past
/// the corners.
pub const VIGNETTE_REACH: f64 = 1.5;

#[cfg(test)]
mod tests {
    use super::*;

    fn adjust(edit: impl FnOnce(&mut Adjust)) -> Adjust {
        let mut adjust = Adjust::default();
        edit(&mut adjust);
        adjust
    }

    #[test]
    fn nothing_given_is_no_grade() {
        assert!(Adjust::default().grade().is_none());
        assert!(adjust(|a| a.exposure = Some(0.0)).is_identity());
        assert!(adjust(|a| a.blur = Some(3.0)).grade().is_none());
    }

    #[test]
    fn exposure_brightens_and_invert_flips() {
        let grade = adjust(|a| a.exposure = Some(1.0)).grade().unwrap();
        let [r, g, b] = grade.rgb([0.2, 0.2, 0.2]);
        assert!(r > 0.25 && (r - g).abs() < 1e-6 && (g - b).abs() < 1e-6);
        let invert = adjust(|a| a.invert = Some(true)).grade().unwrap();
        assert_eq!(invert.colour([255, 0, 10, 128]), [0, 255, 245, 128]);
    }

    #[test]
    fn grayscale_has_no_colour_and_saturation_more() {
        let grey = adjust(|a| a.grayscale = Some(true)).grade().unwrap();
        let [r, g, b] = grey.rgb([0.9, 0.2, 0.1]);
        assert!((r - g).abs() < 1e-3 && (g - b).abs() < 1e-3);
        let vivid = adjust(|a| a.saturation = Some(0.5)).grade().unwrap();
        let [r, _, b] = vivid.rgb([0.6, 0.5, 0.4]);
        assert!(r - b > 0.2);
    }

    #[test]
    fn warmth_lifts_red_over_blue() {
        let warm = adjust(|a| a.temperature = Some(0.5)).grade().unwrap();
        let [r, _, b] = warm.rgb([0.5, 0.5, 0.5]);
        assert!(r > 0.5 && b < 0.5);
    }

    #[test]
    fn a_curve_passes_through_its_points_and_stays_monotone() {
        let curve = Curve::new(&[[0.0, 0.0], [0.25, 0.4], [1.0, 1.0]]);
        assert!((curve.at(0.25) - 0.4).abs() < 1e-4);
        let mut last = 0.0;
        for step in 0..=100 {
            let value = curve.at(step as f32 / 100.0);
            assert!(value + 1e-6 >= last);
            last = value;
        }
    }

    #[test]
    fn merged_edits_keep_what_they_do_not_name() {
        let base = adjust(|a| {
            a.exposure = Some(0.5);
            a.contrast = Some(0.2);
        });
        let merged = base.merged(&adjust(|a| a.contrast = Some(0.0))).unwrap();
        assert_eq!(merged.exposure, Some(0.5));
        assert_eq!(merged.contrast, None);
        assert!(base.merged(&Adjust::default()).is_none());
    }

    #[test]
    fn out_of_range_steps_are_named() {
        let problem = adjust(|a| a.exposure = Some(9.0)).problem().unwrap();
        assert!(problem.contains("exposure"));
        assert!(
            adjust(|a| a.curve = Some(vec![[0.5, 2.0]]))
                .problem()
                .is_some()
        );
    }

    #[test]
    fn blur_keeps_a_flat_picture_and_softens_an_edge() {
        let (width, height) = (32, 8);
        let mut flat = vec![100_u8; width * height * 4];
        blur(&mut flat, width, height, 4.0);
        assert!(flat.iter().all(|value| *value == 100));
        let mut edge: Vec<u8> = (0..width * height)
            .flat_map(|index| {
                let value = if index % width < width / 2 { 0 } else { 255 };
                [value, value, value, 255]
            })
            .collect();
        blur(&mut edge, width, height, 4.0);
        let middle = edge[(width / 2) * 4];
        assert!(middle > 40 && middle < 215, "{middle}");
    }

    #[test]
    fn grading_is_the_same_on_one_thread_or_many() {
        let pixels: Vec<u8> = (0..300_000_u32).map(|value| (value % 251) as u8).collect();
        let grade = adjust(|a| {
            a.exposure = Some(0.3);
            a.hue = Some(40.0);
            a.vibrance = Some(0.4);
        })
        .grade()
        .unwrap();
        let mut many = pixels.clone();
        grade_pixels(&mut many, std::slice::from_ref(&grade));
        let mut one = pixels;
        for pixel in one.chunks_exact_mut(4) {
            let [r, g, b] = grade.rgb([pixel[0], pixel[1], pixel[2]].map(|c| f32::from(c) / 255.0));
            pixel[0] = byte(r);
            pixel[1] = byte(g);
            pixel[2] = byte(b);
        }
        assert_eq!(many, one);
    }
}
