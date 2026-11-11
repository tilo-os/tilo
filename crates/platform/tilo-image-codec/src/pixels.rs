//! What a program does to decoded pixels: crop, turn, scale and grey them,
//! in plain Rust on every target, so no app needs an image library for it.

use crate::DecodedImage;

/// One pixel's four bytes.
type Rgba = [u8; 4];

impl DecodedImage {
    /// `width` by `height` pixels of one colour.
    #[must_use]
    pub fn filled(width: u32, height: u32, rgba: Rgba) -> Self {
        let pixels = (width as usize).saturating_mul(height as usize);
        Self {
            rgba: rgba.repeat(pixels),
            width,
            height,
        }
    }

    /// `rgba` as an image `width` by `height`; `None` when its length is
    /// not `width * height * 4`.
    #[must_use]
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        let expected = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (rgba.len() == expected).then_some(Self {
            rgba,
            width,
            height,
        })
    }

    /// The pixels, row by row.
    pub fn pixels(&self) -> impl Iterator<Item = Rgba> + '_ {
        self.rgba
            .chunks_exact(4)
            .map(|pixel| <Rgba>::try_from(pixel).unwrap_or_default())
    }

    /// The pixel at `x`, `y`; `None` outside the image.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgba> {
        if x >= self.width {
            return None;
        }
        let start = (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)?
            .checked_mul(4)?;
        let pixel = self.rgba.get(start..start.checked_add(4)?)?;
        <Rgba>::try_from(pixel).ok()
    }

    /// Whether any pixel is not fully opaque.
    #[must_use]
    pub fn has_alpha(&self) -> bool {
        self.pixels().any(|pixel| pixel[3] != u8::MAX)
    }

    /// Rows of `self.width` pixels.
    fn rows(&self) -> impl Iterator<Item = &[u8]> {
        self.rgba
            .chunks_exact((self.width as usize).saturating_mul(4).max(4))
    }

    /// The rectangle `x`, `y`, `width` by `height`, clipped to the image;
    /// `None` when nothing of it is inside.
    #[must_use]
    pub fn cropped(&self, x: u32, y: u32, width: u32, height: u32) -> Option<Self> {
        let right = x.saturating_add(width).min(self.width);
        let bottom = y.saturating_add(height).min(self.height);
        let width = right.checked_sub(x).filter(|width| *width > 0)?;
        let height = bottom.checked_sub(y).filter(|height| *height > 0)?;
        let rgba = self
            .rows()
            .skip(y as usize)
            .take(height as usize)
            .flat_map(|row| {
                row.chunks_exact(4)
                    .skip(x as usize)
                    .take(width as usize)
                    .flatten()
                    .copied()
            })
            .collect();
        Some(Self {
            rgba,
            width,
            height,
        })
    }

    /// The image turned clockwise by `quarter_turns` quarter turns.
    #[must_use]
    pub fn turned(&self, quarter_turns: u32) -> Self {
        let (w, h) = (self.width, self.height);
        let turns = quarter_turns % 4;
        let (width, height) = if turns.is_multiple_of(2) {
            (w, h)
        } else {
            (h, w)
        };
        // Each output pixel, row by row, read from where it was.
        let rgba = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let (from_x, from_y) = match turns {
                    0 => (x, y),
                    1 => (y, h.saturating_sub(1).saturating_sub(x)),
                    2 => (
                        w.saturating_sub(1).saturating_sub(x),
                        h.saturating_sub(1).saturating_sub(y),
                    ),
                    _ => (w.saturating_sub(1).saturating_sub(y), x),
                };
                self.pixel(from_x, from_y).unwrap_or_default()
            })
            .collect();
        Self {
            rgba,
            width,
            height,
        }
    }

    /// The image scaled to `width` by `height` with a triangle filter (each
    /// output pixel a weighted mean of the source pixels under it), so a
    /// large reduction keeps its detail rather than aliasing.
    #[must_use]
    pub fn resized(&self, width: u32, height: u32) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) || self.width == 0 || self.height == 0 {
            return self.clone();
        }
        let across = self.resampled_rows(width);
        let turned = across.turned(1).resampled_rows(height);
        turned.turned(3)
    }

    /// Each row resampled to `width` pixels.
    fn resampled_rows(&self, width: u32) -> Self {
        let taps = taps(self.width, width);
        let rgba = self
            .rows()
            .flat_map(|row| {
                taps.iter().flat_map(move |(first, weights)| {
                    let mut sum = [0f32; 4];
                    for (pixel, weight) in row.chunks_exact(4).skip(*first).zip(weights) {
                        for (total, channel) in sum.iter_mut().zip(pixel) {
                            *total += f32::from(*channel) * weight;
                        }
                    }
                    sum.map(channel_byte)
                })
            })
            .collect();
        Self {
            rgba,
            width,
            height: self.height,
        }
    }

    /// The image scaled down, keeping its proportions, so neither side is
    /// longer than `max_dimension`; as it is when it already fits.
    #[must_use]
    pub fn fitted(&self, max_dimension: u32) -> Self {
        match fitted_size(self.width, self.height, max_dimension) {
            Some((width, height)) => self.resized(width, height),
            None => self.clone(),
        }
    }

    /// Each pixel's luminance (Rec. 709 weights), one byte per pixel, alpha
    /// ignored.
    #[must_use]
    pub fn luma(&self) -> Vec<u8> {
        self.pixels()
            .map(|[r, g, b, _]| {
                let weighted = u32::from(r)
                    .saturating_mul(2126)
                    .saturating_add(u32::from(g).saturating_mul(7152))
                    .saturating_add(u32::from(b).saturating_mul(722));
                u8::try_from(weighted.checked_div(10_000).unwrap_or(0)).unwrap_or(u8::MAX)
            })
            .collect()
    }
}

/// The size `width` by `height` scaled down to fit `max_dimension`, keeping
/// its proportions; `None` when it already fits.
#[must_use]
pub(crate) fn fitted_size(width: u32, height: u32, max_dimension: u32) -> Option<(u32, u32)> {
    let max = max_dimension.max(1);
    let longest = width.max(height);
    if longest <= max {
        return None;
    }
    let side = |length: u32| {
        let scaled = u64::from(length)
            .saturating_mul(u64::from(max))
            .saturating_add(u64::from(longest >> 1))
            .checked_div(u64::from(longest))
            .unwrap_or(0);
        u32::try_from(scaled).unwrap_or(max).max(1)
    };
    Some((side(width), side(height)))
}

/// A float channel value as a byte, rounded and clamped.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 first"
)]
fn channel_byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// For each of `to` output positions along a line of `from` source pixels,
/// the first source pixel it reads and the triangle filter's weights from
/// there.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "positions are pixel indices of an image, far below f32's exact range, and clamped to the line"
)]
fn taps(from: u32, to: u32) -> Vec<(usize, Vec<f32>)> {
    let ratio = from as f32 / to.max(1) as f32;
    let support = ratio.max(1.0);
    (0..to)
        .map(|at| {
            let centre = (at as f32 + 0.5) * ratio;
            let first = (centre - support).floor().max(0.0) as u32;
            let last = ((centre + support).ceil().max(0.0) as u32).min(from);
            let mut weights: Vec<f32> = (first..last)
                .map(|source| (1.0 - ((source as f32 + 0.5 - centre) / support).abs()).max(0.0))
                .collect();
            let total: f32 = weights.iter().sum();
            if total > 0.0 {
                for weight in &mut weights {
                    *weight /= total;
                }
            }
            (first as usize, weights)
        })
        .collect()
}
