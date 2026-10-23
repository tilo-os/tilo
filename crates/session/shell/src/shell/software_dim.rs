//! Dimming a display that has no brightness control of its own.
//!
//! A monitor without a backlight the session can drive and without DDC/CI
//! is dimmed by the compositor ([`os_actions::BrightnessTarget::Software`]):
//! the light it shows is scaled in linear light by
//! [`os_actions::software_luminance`], never below its readable floor.
//!
//! Where the display engine has a colour pipeline, the scaling lives there:
//! the CRTC's `GAMMA_LUT`, which applies to every plane after composition,
//! so a fullscreen client scanned out directly is dimmed too and direct
//! scanout keeps working ([`gamma_lut`]). Only a CRTC without a LUT is
//! dimmed in the compositor's own final pass, with dithering
//! ([`final_pass_dim`], [`CpuDim`]); a client buffer flipped onto such a
//! display would skip that pass, so it is composited instead.
//!
//! Either way the dimming happens after the frame the shell paints:
//! screenshots, recordings and screen sharing read that frame, so none of
//! them is dimmed.

use crate::shell::color_pipeline::{PQ_MAX_NITS, pq_eotf, pq_inverse_eotf, srgb_eotf, srgb_oetf};

/// How a display's signal encodes light, which a LUT must decode before it
/// scales light and encode again after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    /// An SDR signal: sRGB.
    Srgb,
    /// An HDR signal: SMPTE ST 2084 (PQ), absolute luminance.
    Pq,
}

/// `light` as the share of linear light a display lets through: `1.0` dims
/// nothing. Anything else (not finite, out of range) dims nothing either.
fn sanitize(light: f32) -> f32 {
    if light.is_finite() {
        light.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

pub fn dims(light: f32) -> bool {
    sanitize(light) < 1.0
}

/// One encoded signal value `x` (`0.0..=1.0`) of a display dimmed to
/// `light`: decoded to linear light, scaled, encoded again.
pub fn dimmed_signal(x: f32, light: f32, transfer: Transfer) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let light = sanitize(light);
    match transfer {
        Transfer::Srgb => srgb_oetf(srgb_eotf(x) * light),
        Transfer::Pq => pq_inverse_eotf((pq_eotf(x) * light).min(PQ_MAX_NITS)),
    }
}

/// The CRTC `GAMMA_LUT` that dims a display to `light`: `size` entries
/// (`GAMMA_LUT_SIZE`), each at the LUT's full 16-bit precision, the same
/// curve on red, green and blue. Entry `i` is the signal `i / (size - 1)`
/// dimmed in linear light ([`dimmed_signal`]). Empty below two entries.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a LUT index is far below f32's exact integer range; values are clamped into 0..=u16::MAX"
)]
pub fn gamma_lut(size: usize, light: f32, transfer: Transfer) -> Vec<u16> {
    if size < 2 {
        return Vec::new();
    }
    let last = size.saturating_sub(1) as f32;
    (0..size)
        .map(|i| {
            let value =
                (dimmed_signal(i as f32 / last, light, transfer) * f32::from(u16::MAX)).round();
            value.clamp(0.0, f32::from(u16::MAX)) as u16
        })
        .collect()
}

/// `lut` as the kernel's `GAMMA_LUT` blob: an array of
/// `struct drm_color_lut { u16 red, green, blue, reserved; }`, native byte
/// order.
pub fn gamma_lut_blob(lut: &[u16]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(lut.len().saturating_mul(8));
    for &value in lut {
        let bytes = value.to_ne_bytes();
        for _channel in 0..3 {
            blob.extend_from_slice(&bytes);
        }
        blob.extend_from_slice(&[0, 0]);
    }
    blob
}

/// The dimming a display's final pass applies: `light`, when it dims and
/// the CRTC carries no LUT that could (`crtc_lut`). `None` otherwise: the
/// CRTC dims it, or nothing does.
pub fn final_pass_dim(light: f32, crtc_lut: bool) -> Option<f32> {
    (dims(light) && !crtc_lut).then(|| sanitize(light))
}

/// The 4×4 Bayer matrix, as thresholds in sixteenths.
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// The final-pass dimming of the CPU (readback) presenter: an sRGB byte
/// dimmed in linear light and rounded with an ordered 4×4 dither, so a
/// dimmed gradient keeps its smoothness instead of banding. One table per
/// threshold, built once per level.
#[derive(Clone, Debug)]
pub struct CpuDim {
    light: f32,
    table: Vec<[u8; 256]>,
}

impl CpuDim {
    /// The tables for `light`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "values are clamped into 0..=255"
    )]
    pub fn new(light: f32) -> Self {
        let light = sanitize(light);
        let table = (0..16_u8)
            .map(|threshold| {
                let offset = (f32::from(threshold) + 0.5) / 16.0;
                let mut row = [0_u8; 256];
                for (code, out) in (0..=255_u8).zip(row.iter_mut()) {
                    let dimmed = dimmed_signal(f32::from(code) / 255.0, light, Transfer::Srgb);
                    *out = (dimmed * 255.0 + offset).floor().clamp(0.0, 255.0) as u8;
                }
                row
            })
            .collect();
        Self { light, table }
    }

    /// The level these tables dim to.
    pub fn light(&self) -> f32 {
        self.light
    }

    /// Dim `rgba` in place: rows of `width` RGBA pixels, alpha untouched.
    pub fn apply(&self, rgba: &mut [u8], width: usize) {
        if width == 0 {
            return;
        }
        for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
            let (x, y) = (
                index.checked_rem(width).unwrap_or(0),
                index.checked_div(width).unwrap_or(0),
            );
            let threshold = BAYER
                .get(y % 4)
                .and_then(|row| row.get(x % 4))
                .copied()
                .unwrap_or(0);
            let Some(table) = self.table.get(usize::from(threshold)) else {
                continue;
            };
            for channel in pixel.iter_mut().take(3) {
                if let Some(&value) = table.get(usize::from(*channel)) {
                    *channel = value;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "endpoints are exact")]
mod tests {
    use super::*;

    fn linear(code: u16) -> f32 {
        srgb_eotf(f32::from(code) / f32::from(u16::MAX))
    }

    #[test]
    fn the_crtc_lut_scales_linear_light_monotonically_at_full_precision() {
        let lut = gamma_lut(1024, 0.5, Transfer::Srgb);
        assert_eq!(lut.len(), 1024, "one entry per GAMMA_LUT_SIZE");
        assert_eq!(lut[0], 0, "black stays black");
        for pair in lut.windows(2) {
            assert!(pair[0] <= pair[1], "monotonic: {pair:?}");
        }
        for (i, &value) in lut.iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "small index")]
            let input = srgb_eotf(i as f32 / 1023.0);
            assert!(
                (linear(value) - input * 0.5).abs() < 1e-3,
                "entry {i}: {} is not half of {input}",
                linear(value)
            );
        }
        // Full precision: neighbouring entries are not stuck on 8-bit steps.
        let distinct = lut.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert!(distinct > 1000, "16-bit entries, {distinct} distinct steps");
        assert_eq!(
            gamma_lut(256, 1.0, Transfer::Srgb).last().copied(),
            Some(u16::MAX),
            "undimmed white is full white"
        );
        assert!(gamma_lut(1, 0.5, Transfer::Srgb).is_empty());
    }

    #[test]
    fn the_lut_never_goes_below_the_software_floor() {
        let floor = os_actions::software_luminance(0.0);
        assert_eq!(floor, os_actions::SOFTWARE_DIM_FLOOR);
        let lut = gamma_lut(256, floor, Transfer::Srgb);
        let white = linear(*lut.last().expect("entries"));
        assert!(
            (white - floor).abs() < 1e-3,
            "white at the bottom of the slider is the floor's light, {white}"
        );
        assert!(white > 0.0, "never black");
    }

    #[test]
    fn a_pq_signal_is_dimmed_in_absolute_light() {
        let lut = gamma_lut(4096, 0.25, Transfer::Pq);
        for pair in lut.windows(2) {
            assert!(pair[0] <= pair[1]);
        }
        let half = dimmed_signal(pq_inverse_eotf(400.0), 0.25, Transfer::Pq);
        assert!((pq_eotf(half) - 100.0).abs() < 0.5, "400 nits become 100");
    }

    #[test]
    fn the_blob_is_the_kernels_drm_color_lut_array() {
        let blob = gamma_lut_blob(&[0x1234, 0xFFFF]);
        assert_eq!(blob.len(), 16, "8 bytes per entry");
        let first = 0x1234_u16.to_ne_bytes();
        assert_eq!(&blob[0..2], &first, "red");
        assert_eq!(&blob[2..4], &first, "green");
        assert_eq!(&blob[4..6], &first, "blue");
        assert_eq!(&blob[6..8], &[0, 0], "reserved");
    }

    #[test]
    fn only_a_crtc_without_a_lut_dims_in_the_final_pass() {
        assert_eq!(final_pass_dim(0.5, true), None, "the CRTC dims it");
        assert_eq!(final_pass_dim(0.5, false), Some(0.5));
        assert_eq!(final_pass_dim(1.0, false), None, "nothing to dim");
        assert_eq!(final_pass_dim(f32::NAN, false), None);
    }

    #[test]
    fn the_cpu_fallback_dithers_a_flat_colour_around_its_dimmed_value() {
        let dim = CpuDim::new(0.5);
        let mut rgba = [200_u8, 200, 200, 77].repeat(16);
        dim.apply(&mut rgba, 4);
        let target = dimmed_signal(200.0 / 255.0, 0.5, Transfer::Srgb) * 255.0;
        let reds: Vec<u8> = rgba.chunks_exact(4).map(|p| p[0]).collect();
        #[allow(clippy::cast_precision_loss, reason = "sixteen bytes")]
        let mean = reds.iter().map(|&r| f32::from(r)).sum::<f32>() / reds.len() as f32;
        assert!(
            (mean - target).abs() < 0.5,
            "mean {mean} is the dimmed {target}"
        );
        assert!(
            reds.iter()
                .all(|&r| f32::from(r) >= target.floor() && f32::from(r) <= target.ceil()),
            "each pixel rounds to a neighbour of the dimmed value: {reds:?}"
        );
        assert!(rgba.chunks_exact(4).all(|p| p[3] == 77), "alpha untouched");
        let mut black = vec![0_u8; 64];
        dim.apply(&mut black, 4);
        assert!(black.iter().all(|&v| v == 0), "black stays black");
    }
}
