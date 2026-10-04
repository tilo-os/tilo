//! Pixel comparison of two premultiplied RGBA8 images, for checking this
//! rasterizer against vello's GPU output.

/// Differences between two equally sized RGBA8 images.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Diff {
    pub pixels: usize,
    /// Largest per-channel difference.
    pub max: u8,
    /// Mean absolute per-channel difference.
    pub mean: f64,
    /// Pixels where some channel differs by more than 8.
    pub over_8: usize,
    /// Pixels where some channel differs by more than 32.
    pub over_32: usize,
    /// Peak signal-to-noise ratio over all channels, in dB; infinite when equal.
    pub psnr: f64,
}

impl Diff {
    /// Share of pixels differing by more than 8 in some channel.
    pub fn over_8_ratio(&self) -> f64 {
        self.over_8 as f64 / self.pixels.max(1) as f64
    }

    /// Share of pixels differing by more than 32 in some channel.
    pub fn over_32_ratio(&self) -> f64 {
        self.over_32 as f64 / self.pixels.max(1) as f64
    }
}

impl std::fmt::Display for Diff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "max {:3}  mean {:.3}  >8 {:.3}%  >32 {:.3}%  psnr {:.1} dB",
            self.max,
            self.mean,
            self.over_8_ratio() * 100.0,
            self.over_32_ratio() * 100.0,
            self.psnr
        )
    }
}

/// Compare `a` and `b`, both RGBA8 of the same length.
pub fn compare(a: &[u8], b: &[u8]) -> Diff {
    let mut diff = Diff::default();
    let mut sum = 0u64;
    let mut sq = 0u64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        diff.pixels += 1;
        let mut px_max = 0u8;
        for (ca, cb) in pa.iter().zip(pb) {
            let d = ca.abs_diff(*cb);
            px_max = px_max.max(d);
            sum += u64::from(d);
            sq += u64::from(d) * u64::from(d);
        }
        diff.max = diff.max.max(px_max);
        diff.over_8 += usize::from(px_max > 8);
        diff.over_32 += usize::from(px_max > 32);
    }
    let channels = (diff.pixels * 4).max(1) as f64;
    diff.mean = sum as f64 / channels;
    let mse = sq as f64 / channels;
    diff.psnr = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    };
    diff
}

/// Premultiply straight-alpha RGBA8 in place (vello's GPU output is straight).
pub fn premultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = u16::from(px[3]);
        for c in &mut px[..3] {
            *c = ((u16::from(*c) * a + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_images_have_no_difference() {
        let a = [10u8, 20, 30, 255, 0, 0, 0, 0];
        let d = compare(&a, &a);
        assert_eq!(d.max, 0);
        assert!(d.psnr.is_infinite());
    }

    #[test]
    fn differences_are_counted_per_pixel() {
        let a = [0u8, 0, 0, 255, 0, 0, 0, 255];
        let b = [40u8, 0, 0, 255, 9, 0, 0, 255];
        let d = compare(&a, &b);
        assert_eq!((d.max, d.over_8, d.over_32), (40, 2, 1));
    }

    #[test]
    fn premultiply_scales_colour_by_alpha() {
        let mut px = [255u8, 128, 0, 128];
        premultiply(&mut px);
        assert_eq!(px, [128, 64, 0, 128]);
    }
}
