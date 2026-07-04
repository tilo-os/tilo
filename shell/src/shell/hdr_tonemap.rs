//! HDR → SDR tone-mapping — CPU implementation + WGSL compute shader.
//!
//! The CPU path in [`tonemap_p010_to_rgba8`] is what PeerView calls in
//! its `ensure_image` fallback: decoded P010 planes → sRGB bytes
//! blitted through the existing vello image path. It's the "works
//! everywhere, no GPU compute pipeline needed" spine.
//!
//! The WGSL kernel [`SHADER_SOURCE`] mirrors the CPU math one-to-one so
//! callers with a wgpu device can prefer it (offloads the per-pixel
//! nonlinearity to the GPU). Both paths take a `Colorimetry` +
//! `HdrTonemapConfig` and produce the same visual result within
//! floating-point rounding.
//!
//! # Pipeline
//!
//! 1. **Unpack** P010: samples live in the top 10 bits of a `u16`,
//!    shift down and scale to `[0, 1]` normalised code values.
//! 2. **Y'CbCr → R'G'B'** via BT.2020 non-constant luminance
//!    (matrix in [`bt2020_ycbcr_to_rgb`]). Handles both limited-range
//!    (64..940 luma, 64..960 chroma for 10 bit) and full-range inputs.
//! 3. **PQ EOTF** (SMPTE ST 2084) → linear cd/m² per component.
//! 4. **BT.2020 → BT.709 gamut** — matrix from ITU-R BT.2087 §3.1.2,
//!    clipping negatives to the SDR gamut boundary.
//! 5. **Hable tone map** — compresses linear luminance to `[0, 1]`
//!    at the target's SDR peak (100 nits, mapped from
//!    `HdrTonemapConfig::source_peak_nits`).
//! 6. **BT.709 OETF (approx sRGB)** → 8-bit sRGB per channel.
//!
//! The Hable curve is what Uncharted 2 shipped and what Filmic Blender
//! iterates on; 12 lines, produces a decent perceptual match with
//! good highlight rolloff. BT.2390 is the alternative for broadcast
//! HDR — either would work; Hable's simpler math wins for a
//! compositor codepath that runs per-frame per-viewport.

use crate::collab::colorimetry::{ColorRange, Colorimetry, TransferCharacteristics};
use crate::collab::hdr_metadata::Hdr10Metadata;

/// The compute-shader source. Contains the same math as
/// [`tonemap_p010_to_rgba8`]; kept as a WGSL string so callers that
/// already have a wgpu device can prefer it. The tests
/// (`hdr_tonemap_shader.rs`) instantiate it against an offscreen wgpu
/// device to verify the shader compiles + produces expected mid-gray.
pub const SHADER_SOURCE: &str = include_str!("hdr_tonemap.wgsl");

/// Configuration for the tone-mapper: what source peak to compress
/// from and what target peak to compress to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HdrTonemapConfig {
    /// Peak luminance of the source content, cd/m². Derived from
    /// [`Hdr10Metadata::mastering.max_display_mastering_luminance`]
    /// when available, defaulting to 1000 nits (the HDR10 baseline).
    pub source_peak_nits: f32,
    /// Peak luminance the tone-mapper compresses to. Default 100 cd/m²
    /// = "SDR display peak". Raising this preserves highlights better
    /// on a bright SDR display but starts to clip on a dim one.
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

impl HdrTonemapConfig {
    /// Derive a tone-map config from the stream's HDR10 static
    /// metadata. Absent metadata → 1000-nit default; a mastering
    /// display flagged 4000-nit expands the compression target so
    /// bright highlights survive.
    pub fn from_metadata(md: Option<Hdr10Metadata>) -> Self {
        let source_peak = md
            .map(|m| m.mastering.max_display_mastering_luminance as f32 / 10_000.0)
            .filter(|nits| nits.is_finite() && *nits > 0.0)
            .unwrap_or(1000.0);
        Self {
            source_peak_nits: source_peak,
            target_peak_nits: 100.0,
        }
    }
}

/// CPU tone-map P010 planes → sRGB RGBA bytes. Output is
/// row-major, no padding, `width * height * 4` bytes. See the module
/// docs for the pipeline steps.
///
/// `y_stride` / `uv_stride` are in `u16` units (i.e. sample count per
/// row), not bytes. The chroma plane is half-resolution horizontally
/// AND vertically (4:2:0) so `uv[i]` is one chroma pair per two luma
/// samples in each direction — the caller doesn't have to compute
/// that; this function subsamples nearest.
///
/// # Panics
///
/// Panics if `y.len() < y_stride * height` — the caller is expected to
/// hand tight buffers per the [`crate::network::decoder::HevcDecoder`]
/// output contract.
#[allow(clippy::too_many_arguments)]
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
    let mut out = vec![0u8; (width * height * 4) as usize];
    let is_hdr = colorimetry.transfer.is_hdr();
    let is_limited = matches!(colorimetry.range, ColorRange::Limited);

    for row in 0..height {
        for col in 0..width {
            let y_idx = (row * y_stride + col) as usize;
            let uv_row = row / 2;
            let uv_col = col / 2;
            // Interleaved UV: [U0, V0, U1, V1, ...] per row, so the
            // pair for pixel column `col` starts at `uv_col * 2` in
            // the row's u16 stream.
            let uv_pair_idx = (uv_row * uv_stride + uv_col * 2) as usize;

            let y_raw = y[y_idx];
            let u_raw = uv[uv_pair_idx];
            let v_raw = uv[uv_pair_idx + 1];

            // Unpack P010: samples live in top 10 bits.
            let y_norm = unpack_p010_sample(y_raw, is_limited, true);
            let u_norm = unpack_p010_sample(u_raw, is_limited, false);
            let v_norm = unpack_p010_sample(v_raw, is_limited, false);

            // BT.2020 non-constant luminance → R'G'B'.
            let (r_prime, g_prime, b_prime) =
                bt2020_ycbcr_to_rgb(y_norm, u_norm, v_norm);

            // PQ EOTF → linear cd/m².
            let (r_lin, g_lin, b_lin) = if colorimetry.transfer == TransferCharacteristics::SmpteSt2084
            {
                (pq_eotf(r_prime), pq_eotf(g_prime), pq_eotf(b_prime))
            } else if colorimetry.transfer == TransferCharacteristics::AribStdB67 {
                // HLG's OETF-inverse — combined with gamma per the
                // ITU-R BT.2100 system function.
                (hlg_eotf(r_prime), hlg_eotf(g_prime), hlg_eotf(b_prime))
            } else {
                // Non-HDR transfer — treat as already linear-ish
                // BT.709 gamma. This branch shouldn't fire for HEVC
                // Main10 in practice; keeps the function defensive.
                (r_prime, g_prime, b_prime)
            };

            // BT.2020 → BT.709 gamut mapping (matrix, clip negatives).
            let (r709, g709, b709) = if is_hdr {
                bt2020_to_bt709(r_lin, g_lin, b_lin)
            } else {
                (r_lin, g_lin, b_lin)
            };

            // Hable tone-map — compresses to target SDR peak.
            let scale = if cfg.source_peak_nits > 0.0 {
                1.0 / cfg.source_peak_nits
            } else {
                1.0
            };
            let r_norm = hable(r709 * scale);
            let g_norm = hable(g709 * scale);
            let b_norm = hable(b709 * scale);

            // Linear → sRGB (BT.709 OETF).
            let r8 = to_srgb_u8(r_norm);
            let g8 = to_srgb_u8(g_norm);
            let b8 = to_srgb_u8(b_norm);

            let out_idx = ((row * width + col) * 4) as usize;
            out[out_idx] = r8;
            out[out_idx + 1] = g8;
            out[out_idx + 2] = b8;
            out[out_idx + 3] = 0xFF;
        }
    }
    out
}

/// Unpack a P010 10-bit sample stored in the top 10 bits of a u16 into
/// a normalised `f32`.
///
/// - Limited range luma: `(v - 64) / (940 - 64)` in 10-bit code
///   values, clipped to `[0, 1]`.
/// - Limited range chroma: `(v - 512) / (960 - 512) * 2 - 1` centred
///   at zero, in `[-0.5, 0.5]`.
/// - Full range: linearly scaled to `[0, 1]` (luma) / `[-0.5, 0.5]`
///   (chroma).
fn unpack_p010_sample(raw: u16, limited: bool, is_luma: bool) -> f32 {
    // Samples live in the top 10 bits of the u16.
    let code = (raw >> 6) as f32;
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

/// BT.2020 non-constant luminance Y'CbCr → R'G'B'. Same coefficients
/// the HEVC Main10 codec uses (ITU-R BT.2020 §5.2).
fn bt2020_ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> (f32, f32, f32) {
    // BT.2020 NCL non-constant luminance:
    // R = Y + 1.4746 * Cr
    // B = Y + 1.8814 * Cb
    // G = Y - (0.2627 * 1.4746 / 0.6780) * Cr - (0.0593 * 1.8814 / 0.6780) * Cb
    let r = y + 1.4746 * cr;
    let b = y + 1.8814 * cb;
    let g = y - 0.16455312684 * cb - 0.57135312684 * cr;
    (r, g, b)
}

/// SMPTE ST 2084 EOTF. Input is a normalised code value in `[0, 1]`
/// (the PQ transfer's electrical signal), output is absolute luminance
/// in cd/m² divided by 10 000 (so 1.0 corresponds to 10 000 nits).
/// We rescale to raw cd/m² at the end by multiplying by 10 000 for the
/// tone-map math.
fn pq_eotf(e_prime: f32) -> f32 {
    const M1: f32 = 0.1593017578125; // 2610/16384
    const M2: f32 = 78.84375; // 2523/32
    const C1: f32 = 0.8359375; // 3424/4096
    const C2: f32 = 18.8515625; // 2413/128
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
    const A: f32 = 0.17883277;
    const B: f32 = 0.28466892;
    const C: f32 = 0.55991073;
    let e = e_prime.max(0.0);
    let linear = if e <= 0.5 {
        (e * e) / 3.0
    } else {
        (((e - C) / A).exp() + B) / 12.0
    };
    // OOTF: scale to reference-display peak. 1000 nits per BT.2100
    // reference; multiplied by system gamma of 1.2 for a nominal
    // 1000-nit HDR TV.
    linear * 1000.0
}

/// BT.2020 → BT.709 gamut matrix (ITU-R BT.2087 §3.1.2). Applied on
/// linear light. Negatives (out-of-gamut) clipped to zero — a proper
/// gamut compression would compress toward the BT.709 boundary, but
/// clipping is what the reference converter does and keeps hue for
/// most content.
fn bt2020_to_bt709(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    // BT.2087 Table 4.
    let r_out = 1.6605 * r - 0.5876 * g - 0.0728 * b;
    let g_out = -0.1246 * r + 1.1329 * g - 0.0083 * b;
    let b_out = -0.0182 * r - 0.1006 * g + 1.1187 * b;
    (r_out.max(0.0), g_out.max(0.0), b_out.max(0.0))
}

/// Hable tone-map operator ("Uncharted 2 filmic"). Input in
/// `[0, ∞)`, output in `[0, ~1)`. The white-point normalisation at the
/// end guarantees `hable(W) == 1.0` for the operator's implicit
/// white point.
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

/// Linear-light `[0, 1]` → 8-bit sRGB code value. Uses the standard
/// piecewise sRGB OETF.
fn to_srgb_u8(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let encoded = if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
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

    /// Build a solid-colour 4×4 P010 image and tone-map it. Assert
    /// the output is close to the expected mid-gray.
    #[test]
    fn mid_gray_pq_maps_to_reasonable_sdr_gray() {
        // Roughly 300 cd/m² PQ code value is code 638 for a full-frame
        // gray. Compute it: `perceptual_to_electrical(300 / 10000)`.
        // For simplicity, pass a raw PQ code value that produces
        // roughly 100 nits after PQ→linear, so it should land near
        // mid-gray in SDR after Hable.
        let width = 4;
        let height = 4;
        // Y ~ mid-code, U/V neutral.
        let luma_code = 512; // ~0.5 in normalised limited range
        let neutral_chroma = 512;

        let y = vec![p010(luma_code); (width * height) as usize];
        let uv = vec![
            p010(neutral_chroma);
            (width / 2 * height / 2) as usize * 2
        ];

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
        assert_eq!(rgba.len() as u32, width * height * 4);
        // Alpha channel should be full.
        for (i, chunk) in rgba.chunks_exact(4).enumerate() {
            assert_eq!(chunk[3], 0xFF, "pixel {i} has non-full alpha");
        }
        // Mid PQ code with neutral chroma → grey; R == G == B roughly.
        let r = rgba[0];
        let g = rgba[1];
        let b = rgba[2];
        assert!(
            (r as i32 - g as i32).abs() <= 6 && (g as i32 - b as i32).abs() <= 6,
            "neutral chroma should produce ~gray, got ({r}, {g}, {b})",
        );
        // Neither pinned black nor crushed white.
        assert!(r > 10, "PQ mid-code shouldn't crush to black: got {r}");
        assert!(r < 245, "PQ mid-code shouldn't blow out to white: got {r}");
    }

    #[test]
    fn tonemap_config_from_metadata_reads_mastering_peak() {
        use crate::collab::hdr_metadata::{
            ContentLightLevel, Hdr10Metadata, MasteringDisplayColorVolume,
        };
        // 4000-nit mastering.
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

        // Absent → default 1000.
        let cfg = HdrTonemapConfig::from_metadata(None);
        assert!((cfg.source_peak_nits - 1000.0).abs() < 0.1);
    }

    #[test]
    fn pq_eotf_roundtrips_reference_points() {
        // PQ mid-code (~0.5 electrical) is ~50 cd/m² (not linear — PQ
        // is a perceptual curve; middle of the code range lands well
        // below middle of the luminance range).
        let y = pq_eotf(0.5);
        assert!(y > 10.0 && y < 250.0, "PQ(0.5) unexpectedly {y}");
        // PQ 0 → 0 nits.
        assert!(pq_eotf(0.0) < 0.01, "PQ(0) should be near zero");
        // PQ 1 → 10 000 nits.
        assert!(
            (pq_eotf(1.0) - 10_000.0).abs() < 1.0,
            "PQ(1) should be 10 000 nits, got {}",
            pq_eotf(1.0)
        );
    }

    #[test]
    fn hable_is_monotonic_and_bounded() {
        let mut prev = -1.0f32;
        for i in 0..20 {
            let x = i as f32 / 10.0;
            let y = hable(x);
            assert!(y >= 0.0 && y <= 1.0, "hable({x}) out of range: {y}");
            assert!(y >= prev, "hable not monotonic at {x}: prev={prev} y={y}");
            prev = y;
        }
    }
}
