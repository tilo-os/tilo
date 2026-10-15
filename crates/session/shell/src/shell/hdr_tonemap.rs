//! HDR → SDR tone-mapping — CPU implementation + WGSL compute shader.
//!
//! [`tonemap_p010_to_rgba8`] is `PeerView`'s `ensure_image` fallback: decoded
//! P010 → sRGB bytes blitted through the vello image path, no GPU compute
//! pipeline needed. [`SHADER_SOURCE`] mirrors the same math for callers with
//! a wgpu device.
//!
//! # Pipeline
//!
//! Unpack P010 (top 10 bits of each `u16`) → BT.2020 non-constant-luminance
//! Y'`CbCr` → R'G'B' (`bt2020_ycbcr_to_rgb`) → PQ/HLG EOTF to linear cd/m²
//! → BT.2020→BT.709 gamut (ITU-R BT.2087 §3.1.2, negatives clipped) →
//! Hable tone-map to `[0,1]` at the target peak → BT.709/sRGB OETF to 8-bit.
//!
//! Hable over BT.2390 (the broadcast-HDR alternative): simpler math wins in
//! a codepath that runs per-frame per-viewport.

use crate::collab::colorimetry::{
    ColorPrimaries, ColorRange, Colorimetry, TransferCharacteristics,
};
use crate::collab::hdr_metadata::Hdr10Metadata;

/// Compute-shader source, same math as [`tonemap_p010_to_rgba8`]. Tests in
/// `hdr_tonemap_shader.rs` compile it on an offscreen wgpu device and check
/// mid-gray output.
pub const SHADER_SOURCE: &str = include_str!("hdr_tonemap.wgsl");

/// Configuration for the tone-mapper: what source peak to compress
/// from and what target peak to compress to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrTonemapConfig {
    /// Peak luminance of the source content, cd/m². Derived from
    /// `Hdr10Metadata::mastering.max_display_mastering_luminance`,
    /// defaulting to 1000 nits (the HDR10 baseline).
    pub source_peak_nits: f32,
    /// Peak luminance to compress to. Default 100 cd/m² ("SDR display
    /// peak"); raising it preserves highlights better on a bright SDR
    /// display but starts to clip on a dim one.
    pub target_peak_nits: f32,
}

impl Default for HdrTonemapConfig {
    fn default() -> Self {
        Self {
            source_peak_nits: 1000.0,
            target_peak_nits: 100.0,
        }
    }
}

/// Convert a mastering-luminance value (`1/10000 cd/m²`, per the HDR10
/// stream metadata encoding) to nits. Stream values top out around
/// `40_000_000` (4000 cd/m²), far inside `f32`'s 24-bit exact-integer
/// range, so the precision loss this cast could otherwise cause cannot
/// happen for real content, and the tone-mapper only needs an
/// approximate peak anyway.
#[allow(
    clippy::cast_precision_loss,
    reason = "mastering luminance stays well under 2^24 for real streams; exact precision isn't needed for a tone-map peak"
)]
fn mastering_luminance_to_nits(raw: u32) -> f32 {
    raw as f32 / 10_000.0
}

impl HdrTonemapConfig {
    /// Derive from the stream's HDR10 static metadata. Absent metadata
    /// falls back to the 1000-nit default.
    pub fn from_metadata(md: Option<Hdr10Metadata>) -> Self {
        let source_peak = md
            .map(|m| mastering_luminance_to_nits(m.mastering.max_display_mastering_luminance))
            .filter(|nits| nits.is_finite() && *nits > 0.0)
            .unwrap_or(1000.0);
        Self {
            source_peak_nits: source_peak,
            target_peak_nits: 100.0,
        }
    }
}

/// CPU tone-map P010 planes → sRGB RGBA bytes, row-major, no padding,
/// `width * height * 4` bytes. See the module docs for the pipeline.
///
/// `y_stride` / `uv_stride` are in `u16` units, not bytes. The chroma
/// plane is 4:2:0 (half-res both axes); this function subsamples nearest.
///
/// The caller hands tight buffers per the
/// [`tilo_callstream::surface_decoder::VideoDecoder`] 10-bit output contract
/// (`y.len() >= y_stride * height`); a shorter buffer never panics —
/// pixels whose sample would fall outside `y`/`uv` are left as
/// transparent black instead.
pub fn tonemap_p010_to_rgba8(
    y: &[u16],
    uv: &[u16],
    y_stride: u32,
    uv_stride: u32,
    width: u32,
    height: u32,
    colorimetry: Colorimetry,
    cfg: HdrTonemapConfig,
) -> Vec<u8> {
    let pixel_count = (width as usize).saturating_mul(height as usize);
    let mut out = vec![0u8; pixel_count.saturating_mul(4)];
    let is_hdr = colorimetry.transfer.is_hdr();
    let is_limited = matches!(colorimetry.range, ColorRange::Limited);

    for row in 0..height {
        for col in 0..width {
            let y_idx = plane_index(row, y_stride, col);
            let uv_row = row / 2;
            let uv_col = col / 2;
            // Interleaved UV: [U0, V0, U1, V1, ...] per row.
            let uv_pair_idx = plane_index(uv_row, uv_stride, uv_col.saturating_mul(2));

            // A malformed or truncated decoder buffer (shorter than its
            // stride implies) leaves this pixel as the zero-initialised
            // transparent black `out` already has, rather than panicking
            // this panic=abort process.
            let (Some(&y_raw), Some(&u_raw), Some(&v_raw)) = (
                y.get(y_idx),
                uv.get(uv_pair_idx),
                uv.get(uv_pair_idx.saturating_add(1)),
            ) else {
                continue;
            };

            let y_norm = unpack_p010_sample(y_raw, is_limited, true);
            let u_norm = unpack_p010_sample(u_raw, is_limited, false);
            let v_norm = unpack_p010_sample(v_raw, is_limited, false);

            let (r_prime, g_prime, b_prime) = bt2020_ycbcr_to_rgb(y_norm, u_norm, v_norm);

            let (r_lin, g_lin, b_lin) =
                if colorimetry.transfer == TransferCharacteristics::SmpteSt2084 {
                    (pq_eotf(r_prime), pq_eotf(g_prime), pq_eotf(b_prime))
                } else if colorimetry.transfer == TransferCharacteristics::AribStdB67 {
                    (hlg_eotf(r_prime), hlg_eotf(g_prime), hlg_eotf(b_prime))
                } else {
                    // Shouldn't fire for HEVC Main10 in practice; treat as
                    // already linear-ish BT.709 rather than panic.
                    (r_prime, g_prime, b_prime)
                };

            let (r709, g709, b709) = if is_hdr {
                bt2020_to_bt709(r_lin, g_lin, b_lin)
            } else {
                (r_lin, g_lin, b_lin)
            };

            let scale = if cfg.source_peak_nits > 0.0 {
                1.0 / cfg.source_peak_nits
            } else {
                1.0
            };
            let r_norm = hable(r709 * scale);
            let g_norm = hable(g709 * scale);
            let b_norm = hable(b709 * scale);

            let r8 = to_srgb_u8(r_norm);
            let g8 = to_srgb_u8(g_norm);
            let b8 = to_srgb_u8(b_norm);

            let out_idx = plane_index(row, width, col).saturating_mul(4);
            if let Some(px) = out.get_mut(out_idx..out_idx.saturating_add(4)) {
                px.copy_from_slice(&[r8, g8, b8, 0xFF]);
            }
        }
    }
    out
}

/// Tone-map one transfer-encoded RGB pixel (`[0,1]` signal in
/// `colorimetry`'s primaries) to sRGB bytes. An HDR signal is linearised by
/// its EOTF, gamut-mapped to BT.709 and compressed against the source peak;
/// linear light clips at SDR white; an SDR signal passes through at 8 bits.
pub fn tonemap_rgb_signal_to_srgb8(
    rgb: [f32; 3],
    colorimetry: Colorimetry,
    cfg: HdrTonemapConfig,
) -> [u8; 3] {
    match colorimetry.transfer {
        TransferCharacteristics::SmpteSt2084
        | TransferCharacteristics::AribStdB67
        | TransferCharacteristics::Linear => {
            tonemap_rgb_signal_to_sdr_linear(rgb, colorimetry, cfg).map(to_srgb_u8)
        }
        TransferCharacteristics::Bt709
        | TransferCharacteristics::Srgb
        | TransferCharacteristics::Unspecified => {
            rgb.map(|c| clamped_f32_to_u8((c * 255.0).round().clamp(0.0, 255.0)))
        }
    }
}

/// [`tonemap_rgb_signal_to_srgb8`] before the sRGB encode: linear BT.709
/// light relative to SDR white, `[0,1]`. What an SDR output composites.
pub fn tonemap_rgb_signal_to_sdr_linear(
    rgb: [f32; 3],
    colorimetry: Colorimetry,
    cfg: HdrTonemapConfig,
) -> [f32; 3] {
    let [r, g, b] = rgb;
    let eotf: fn(f32) -> f32 = match colorimetry.transfer {
        TransferCharacteristics::SmpteSt2084 => pq_eotf,
        TransferCharacteristics::AribStdB67 => hlg_eotf,
        TransferCharacteristics::Linear => {
            let linear = if colorimetry.primaries == ColorPrimaries::Bt2020 {
                let (r, g, b) = bt2020_to_bt709(r, g, b);
                [r, g, b]
            } else {
                [r, g, b]
            };
            return linear.map(|c| c.clamp(0.0, 1.0));
        }
        TransferCharacteristics::Bt709
        | TransferCharacteristics::Srgb
        | TransferCharacteristics::Unspecified => {
            return [r, g, b].map(crate::shell::color_pipeline::srgb_eotf);
        }
    };
    let (r709, g709, b709) = bt2020_to_bt709(eotf(r), eotf(g), eotf(b));
    let scale = if cfg.source_peak_nits > 0.0 {
        1.0 / cfg.source_peak_nits
    } else {
        1.0
    };
    [r709, g709, b709].map(|c| hable(c * scale))
}

/// A P010 frame's two sample planes (BT.2020 `Y'CbCr`, 10 bits in the top of
/// each sample, chroma subsampled 2×2), their strides in samples, and whether
/// the codes are limited range.
#[derive(Clone, Copy)]
pub struct P010Planes<'a> {
    pub luma: &'a [u16],
    pub chroma: &'a [u16],
    pub luma_stride: u32,
    pub chroma_stride: u32,
    pub limited: bool,
}

impl P010Planes<'_> {
    /// The normalised R'G'B' signal of pixel `(col, row)`, chroma sampled
    /// nearest. `None` when the planes are too short.
    pub fn signal_at(&self, col: u32, row: u32) -> Option<[f32; 3]> {
        let y_raw = *self.luma.get(plane_index(row, self.luma_stride, col))?;
        let uv_pair = plane_index(row / 2, self.chroma_stride, (col / 2).saturating_mul(2));
        let u_raw = *self.chroma.get(uv_pair)?;
        let v_raw = *self.chroma.get(uv_pair.saturating_add(1))?;
        let (r, g, b) = bt2020_ycbcr_to_rgb(
            unpack_p010_sample(y_raw, self.limited, true),
            unpack_p010_sample(u_raw, self.limited, false),
            unpack_p010_sample(v_raw, self.limited, false),
        );
        Some([r, g, b].map(|c| c.clamp(0.0, 1.0)))
    }
}

/// Row-major linear index into a stride-`stride` plane at (`row`,
/// `col`). Computed in `u64` so a large `row`/`stride`/`col` combination
/// saturates instead of wrapping, then saturated into `usize` — an
/// out-of-range result simply misses every `.get()` bounds check at the
/// call site instead of panicking.
fn plane_index(row: u32, stride: u32, col: u32) -> usize {
    let idx = u64::from(row)
        .saturating_mul(u64::from(stride))
        .saturating_add(u64::from(col));
    usize::try_from(idx).unwrap_or(usize::MAX)
}

/// Unpack a P010 10-bit sample (top 10 bits of a u16) into a normalised
/// `f32`. Limited-range luma is `64..940`, chroma `64..960` (10-bit code
/// values, per spec); full range is linear `[0,1]`/`[-0.5,0.5]`.
fn unpack_p010_sample(raw: u16, limited: bool, is_luma: bool) -> f32 {
    let code = f32::from(raw >> 6);
    if is_luma {
        if limited {
            ((code - 64.0) / (940.0 - 64.0)).clamp(0.0, 1.0)
        } else {
            (code / 1023.0).clamp(0.0, 1.0)
        }
    } else if limited {
        ((code - 512.0) / (960.0 - 64.0)).clamp(-0.5, 0.5)
    } else {
        (code / 1023.0 - 0.5).clamp(-0.5, 0.5)
    }
}

/// BT.2020 non-constant luminance Y'`CbCr` → R'G'B'. Same coefficients
/// the HEVC Main10 codec uses (ITU-R BT.2020 §5.2).
fn bt2020_ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> (f32, f32, f32) {
    let r = y + 1.4746 * cr;
    let b = y + 1.8814 * cb;
    let g = y - 0.164_553_12 * cb - 0.571_353_14 * cr;
    (r, g, b)
}

/// SMPTE ST 2084 EOTF: PQ electrical signal `[0,1]` → absolute
/// luminance in cd/m² (`pq_eotf(1.0) == 10_000`).
fn pq_eotf(e_prime: f32) -> f32 {
    const M1: f32 = 0.159_301_76; // 2610/16384
    const M2: f32 = 78.84375; // 2523/32
    const C1: f32 = 0.835_937_5; // 3424/4096
    const C2: f32 = 18.851_563; // 2413/128
    const C3: f32 = 18.6875; // 2392/128
    let e = e_prime.max(0.0);
    let e_p = e.powf(1.0 / M2);
    let num = (e_p - C1).max(0.0);
    let den = C2 - C3 * e_p;
    let y = (num / den).powf(1.0 / M1);
    y * 10_000.0 // Convert to cd/m²
}

/// ITU-R BT.2100 HLG EOTF (system function). Input `[0, 1]`, output
/// cd/m² assuming a 1000-nit reference display.
fn hlg_eotf(e_prime: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 0.284_668_92;
    const C: f32 = 0.559_910_7;
    let e = e_prime.max(0.0);
    let linear = if e <= 0.5 {
        (e * e) / 3.0
    } else {
        (((e - C) / A).exp() + B) / 12.0
    };
    // OOTF: scale to the BT.2100 1000-nit reference-display peak.
    linear * 1000.0
}

/// BT.2020 → BT.709 gamut matrix (ITU-R BT.2087 §3.1.2), linear light.
/// Negatives clipped to zero rather than gamut-compressed — matches the
/// reference converter and keeps hue for most content.
fn bt2020_to_bt709(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    // BT.2087 Table 4.
    let r_out = 1.6605 * r - 0.5876 * g - 0.0728 * b;
    let g_out = -0.1246 * r + 1.1329 * g - 0.0083 * b;
    let b_out = -0.0182 * r - 0.1006 * g + 1.1187 * b;
    (r_out.max(0.0), g_out.max(0.0), b_out.max(0.0))
}

/// Hable tone-map operator ("Uncharted 2 filmic"), `[0,∞) -> [0,~1)`.
/// White-point normalisation guarantees `hable(W) == 1.0`.
fn hable(x: f32) -> f32 {
    const A: f32 = 0.15;
    const B: f32 = 0.50;
    const C: f32 = 0.10;
    const D: f32 = 0.20;
    const E: f32 = 0.02;
    const F: f32 = 0.30;
    const W: f32 = 11.2; // exposure whitepoint
    let curve = |v: f32| ((v * (A * v + C * B) + D * E) / (v * (A * v + B) + D * F)) - E / F;
    (curve(x) / curve(W)).clamp(0.0, 1.0)
}

/// Linear-light `[0, 1]` → 8-bit sRGB code value (piecewise sRGB OETF).
fn to_srgb_u8(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let encoded = if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    clamped_f32_to_u8((encoded * 255.0).round().clamp(0.0, 255.0))
}

/// `v` is clamped into `[0.0, 255.0]` by every caller immediately before
/// this is called, so the truncation and sign-loss this cast could
/// otherwise cause cannot happen.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers clamp v into [0.0, 255.0] right before calling"
)]
fn clamped_f32_to_u8(v: f32) -> u8 {
    v as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::colorimetry::{
        ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
    };

    /// Encode a `code_value` (0..1023) as the top-10-bit P010 sample.
    fn p010(code: u16) -> u16 {
        code << 6
    }

    #[test]
    fn hdr_tonemap_maps_pq_to_sdr_from_mastering_metadata_monotonically() {
        /// A mid-code PQ value lands well below mid-luminance (PQ is
        /// perceptual), so this should land near mid-gray after Hable.
        fn mid_gray_pq_maps_to_reasonable_sdr_gray() {
            let width = 4;
            let height = 4;
            let luma_code = 512; // ~0.5 in normalised limited range
            let neutral_chroma = 512;

            let y = vec![p010(luma_code); (width * height) as usize];
            let uv = vec![p010(neutral_chroma); (width / 2 * height / 2) as usize * 2];

            let colorimetry = Colorimetry {
                primaries: ColorPrimaries::Bt2020,
                transfer: TransferCharacteristics::SmpteSt2084,
                matrix: MatrixCoefficients::Bt2020NonConstant,
                range: ColorRange::Limited,
            };
            let cfg = HdrTonemapConfig {
                source_peak_nits: 1000.0,
                target_peak_nits: 100.0,
            };
            let rgba = tonemap_p010_to_rgba8(
                &y,
                &uv,
                width,
                width, // uv stride = width for 4:2:0 interleaved
                width,
                height,
                colorimetry,
                cfg,
            );
            assert_eq!(rgba.len(), (width * height * 4) as usize);
            for (i, chunk) in rgba.chunks_exact(4).enumerate() {
                assert_eq!(chunk[3], 0xFF, "pixel {i} has non-full alpha");
            }
            let r = rgba[0];
            let g = rgba[1];
            let b = rgba[2];
            assert!(
                (i32::from(r) - i32::from(g)).abs() <= 6
                    && (i32::from(g) - i32::from(b)).abs() <= 6,
                "neutral chroma should produce ~gray, got ({r}, {g}, {b})",
            );
            assert!(r > 10, "PQ mid-code shouldn't crush to black: got {r}");
            assert!(r < 245, "PQ mid-code shouldn't blow out to white: got {r}");
        }

        fn tonemap_config_from_metadata_reads_mastering_peak() {
            use crate::collab::hdr_metadata::{
                ContentLightLevel, Hdr10Metadata, MasteringDisplayColorVolume,
            };
            let md = Hdr10Metadata {
                mastering: MasteringDisplayColorVolume {
                    display_primaries: [(0, 0); 3],
                    white_point: (0, 0),
                    max_display_mastering_luminance: 40_000_000, // 4000 cd/m² in 1/10000 units
                    min_display_mastering_luminance: 500,
                },
                light: ContentLightLevel::unknown(),
            };
            let cfg = HdrTonemapConfig::from_metadata(Some(md));
            assert!((cfg.source_peak_nits - 4000.0).abs() < 0.1);

            let cfg = HdrTonemapConfig::from_metadata(None); // absent -> default 1000
            assert!((cfg.source_peak_nits - 1000.0).abs() < 0.1);
        }

        fn pq_eotf_roundtrips_reference_points() {
            // PQ is perceptual — mid-code lands well below mid-luminance.
            let y = pq_eotf(0.5);
            assert!(y > 10.0 && y < 250.0, "PQ(0.5) unexpectedly {y}");
            assert!(pq_eotf(0.0) < 0.01, "PQ(0) should be near zero");
            assert!(
                (pq_eotf(1.0) - 10_000.0).abs() < 1.0,
                "PQ(1) should be 10 000 nits, got {}",
                pq_eotf(1.0)
            );
        }

        fn hable_is_monotonic_and_bounded() {
            let mut prev = -1.0f32;
            for i in 0..20_i16 {
                let x = f32::from(i) / 10.0;
                let y = hable(x);
                assert!((0.0..=1.0).contains(&y), "hable({x}) out of range: {y}");
                assert!(y >= prev, "hable not monotonic at {x}: prev={prev} y={y}");
                prev = y;
            }
        }

        mid_gray_pq_maps_to_reasonable_sdr_gray();
        tonemap_config_from_metadata_reads_mastering_peak();
        pq_eotf_roundtrips_reference_points();
        hable_is_monotonic_and_bounded();
    }
}
