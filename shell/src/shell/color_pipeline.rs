//! Output colour pipeline — the compositor's HDR/SDR composition maths.
//!
//! This module is the **output** counterpart to
//! [`crate::shell::hdr_tonemap`]. Where `hdr_tonemap` decodes an HDR
//! stream *down* to SDR sRGB (the receive/screen-share path), this
//! module is the maths for driving a *real output* — deciding what
//! colour space a monitor is scanned out in and how each surface's
//! pixels are transformed into it.
//!
//! Concretely it supplies the four pieces the compositor needs to
//! composite mixed SDR + HDR content onto one output:
//!
//! 1. **Transfer functions, both directions.** `hdr_tonemap` only ever
//!    needed PQ/HLG *decode* (electrical → linear). Driving an HDR
//!    panel needs the *encode* direction too (linear nits → PQ signal),
//!    so [`pq_inverse_eotf`] / [`hlg_oetf`] live here alongside the
//!    forward [`pq_eotf`] / [`hlg_inverse_oetf`].
//! 2. **Gamut matrices, both directions.** Derived from CIE-1931
//!    primaries via [`rgb_to_xyz`] rather than hand-transcribed, so
//!    BT.709→BT.2020 (map SDR into the HDR container) and
//!    BT.2020→BT.709 (the tone-map path's gamut step) come from one
//!    source of truth and are checked against the ITU reference
//!    matrices in tests.
//! 3. **SDR → HDR mapping.** An SDR surface composited onto an HDR
//!    output must be *mapped*, not clipped: sRGB white lands at the
//!    BT.2408 reference diffuse-white level ([`SDR_REFERENCE_WHITE_NITS`]
//!    = 203 cd/m²), not at the panel peak, and the gamut is expanded
//!    BT.709 → BT.2020. See [`map_sdr_pixel_to_pq_bt2020`].
//! 4. **Per-surface composition decisions.** Given the output's colour
//!    state and a surface's declared [`Colorimetry`],
//!    [`plan_surface_op`] picks passthrough / tone-map / SDR-map /
//!    HLG→PQ transcode.
//!
//! # What is testable here vs. what needs hardware
//!
//! Everything in this module is pure maths and is unit-tested against
//! reference points (PQ/HLG round-trips, the ITU BT.2087/BT.2020
//! matrices, the 203-nit white level). What it *cannot* verify headless
//! is the last mile: putting a DRM connector into an HDR signalling
//! mode (10-bit framebuffer + `HDR_OUTPUT_METADATA` connector property)
//! and scanning out `Rgb10a2`/`Rgba16Float` to a real HDR panel. Those
//! need a physical HDR display and a KMS/Vulkan HDR-capable stack; the
//! selection logic ([`OutputColorState::from_display`],
//! [`OutputColorState::preferred_swapchain_format`]) is wired and
//! tested, but the pixels only leave the box correctly on real HDR
//! hardware.

use crate::collab::colorimetry::{ColorPrimaries, Colorimetry, TransferCharacteristics};
use crate::shell::hdr_display::DisplayCapabilities;
use crate::shell::hdr_tonemap::HdrTonemapConfig;

/// WGSL source for the SDR → HDR output-encode compute shader — the GPU
/// mirror of [`map_sdr_pixel_to_pq_bt2020`]. Kept as a string so a
/// backend with a wgpu device can dispatch it when an output is driven
/// in HDR PQ mode; the tests validate it through `naga`.
pub const OUTPUT_ENCODE_SHADER_SOURCE: &str = include_str!("hdr_output_encode.wgsl");

/// BT.2408 "HDR reference white" — the luminance a nominal SDR diffuse
/// white (sRGB 1.0, "graphics white") should map to when composited
/// into an HDR (PQ/HLG) signal. Mapping SDR white to the *panel peak*
/// would make the desktop searingly bright next to HDR video; 203 cd/m²
/// is the ITU-agreed diffuse-white level that keeps SDR UI looking
/// "normal" beside HDR highlights.
pub const SDR_REFERENCE_WHITE_NITS: f32 = 203.0;

/// PQ peak luminance — the absolute-luminance ceiling ST 2084 encodes.
pub const PQ_MAX_NITS: f32 = 10_000.0;

// ---------------------------------------------------------------------
// 3×3 linear-algebra — just enough for colour-space matrices.
// ---------------------------------------------------------------------

/// A 3×3 matrix in row-major order. Small, `Copy`, and only carries the
/// handful of operations colour-space conversion needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3 {
    /// `m[row][col]`.
    pub m: [[f32; 3]; 3],
}

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3 {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };

    /// Matrix · column-vector.
    pub fn mul_vec(&self, v: [f32; 3]) -> [f32; 3] {
        [
            self.m[0][0] * v[0] + self.m[0][1] * v[1] + self.m[0][2] * v[2],
            self.m[1][0] * v[0] + self.m[1][1] * v[1] + self.m[1][2] * v[2],
            self.m[2][0] * v[0] + self.m[2][1] * v[1] + self.m[2][2] * v[2],
        ]
    }

    /// Matrix · matrix.
    pub fn mul(&self, rhs: &Mat3) -> Mat3 {
        let mut out = [[0.0f32; 3]; 3];
        for (r, out_row) in out.iter_mut().enumerate() {
            for (c, out_cell) in out_row.iter_mut().enumerate() {
                *out_cell = self.m[r][0] * rhs.m[0][c]
                    + self.m[r][1] * rhs.m[1][c]
                    + self.m[r][2] * rhs.m[2][c];
            }
        }
        Mat3 { m: out }
    }

    /// Determinant (used by [`Self::inverse`]).
    pub fn det(&self) -> f32 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    /// Matrix inverse, or `None` for a (near-)singular matrix. Colour
    /// primary matrices are always well-conditioned, so `None` only
    /// happens on a programming error.
    pub fn inverse(&self) -> Option<Mat3> {
        let det = self.det();
        if det.abs() < 1e-12 {
            return None;
        }
        let inv_det = 1.0 / det;
        let m = &self.m;
        // Cofactor / adjugate method.
        let c = [
            [
                (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv_det,
                (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det,
                (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det,
            ],
            [
                (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv_det,
                (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det,
                (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det,
            ],
            [
                (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv_det,
                (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det,
                (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det,
            ],
        ];
        Some(Mat3 { m: c })
    }

    /// Max absolute element-wise difference — for tests / tolerances.
    pub fn max_abs_diff(&self, other: &Mat3) -> f32 {
        let mut worst = 0.0f32;
        for r in 0..3 {
            for col in 0..3 {
                worst = worst.max((self.m[r][col] - other.m[r][col]).abs());
            }
        }
        worst
    }
}

// ---------------------------------------------------------------------
// Primaries → RGB↔XYZ, and the gamut conversion matrices.
// ---------------------------------------------------------------------

/// CIE-1931 xy chromaticities of an RGB colour space: the R/G/B primary
/// points and the reference white.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chromaticities {
    pub red: (f32, f32),
    pub green: (f32, f32),
    pub blue: (f32, f32),
    pub white: (f32, f32),
}

/// BT.709 / sRGB primaries with the D65 white point.
pub const BT709_PRIMARIES: Chromaticities = Chromaticities {
    red: (0.640, 0.330),
    green: (0.300, 0.600),
    blue: (0.150, 0.060),
    white: (0.3127, 0.3290),
};

/// BT.2020 primaries with the D65 white point.
pub const BT2020_PRIMARIES: Chromaticities = Chromaticities {
    red: (0.708, 0.292),
    green: (0.170, 0.797),
    blue: (0.131, 0.046),
    white: (0.3127, 0.3290),
};

/// Display-P3 primaries with the D65 white point.
pub const DISPLAY_P3_PRIMARIES: Chromaticities = Chromaticities {
    red: (0.680, 0.320),
    green: (0.265, 0.690),
    blue: (0.150, 0.060),
    white: (0.3127, 0.3290),
};

/// Build the linear RGB → CIE-XYZ matrix for a set of primaries, using
/// the standard construction (SMPTE RP 177): convert each chromaticity
/// to XYZ with `Y = 1`, solve for the per-primary luminance scalars so
/// the white point maps to `(Xw, 1, Zw)`, then scale the primary
/// columns. The inverse of this matrix is XYZ → RGB.
pub fn rgb_to_xyz(p: &Chromaticities) -> Mat3 {
    let xyz = |(x, y): (f32, f32)| -> [f32; 3] { [x / y, 1.0, (1.0 - x - y) / y] };
    let xr = xyz(p.red);
    let xg = xyz(p.green);
    let xb = xyz(p.blue);
    // Columns are the primaries.
    let primary_mat = Mat3 {
        m: [
            [xr[0], xg[0], xb[0]],
            [xr[1], xg[1], xb[1]],
            [xr[2], xg[2], xb[2]],
        ],
    };
    let w = xyz(p.white);
    let s = primary_mat
        .inverse()
        .expect("primary matrix is non-singular for physical primaries")
        .mul_vec(w);
    // Scale each primary column by its luminance scalar.
    Mat3 {
        m: [
            [xr[0] * s[0], xg[0] * s[1], xb[0] * s[2]],
            [xr[1] * s[0], xg[1] * s[1], xb[1] * s[2]],
            [xr[2] * s[0], xg[2] * s[1], xb[2] * s[2]],
        ],
    }
}

/// Linear gamut-conversion matrix from `src` primaries to `dst`
/// primaries (no chromatic adaptation — both use the same D65 white in
/// practice, so none is needed).
pub fn gamut_matrix(src: &Chromaticities, dst: &Chromaticities) -> Mat3 {
    let src_to_xyz = rgb_to_xyz(src);
    let dst_to_xyz = rgb_to_xyz(dst);
    let xyz_to_dst = dst_to_xyz
        .inverse()
        .expect("dst RGB→XYZ is invertible for physical primaries");
    xyz_to_dst.mul(&src_to_xyz)
}

/// Linear BT.709 → BT.2020 gamut matrix (map SDR content into the HDR
/// wide-gamut container).
pub fn bt709_to_bt2020_matrix() -> Mat3 {
    gamut_matrix(&BT709_PRIMARIES, &BT2020_PRIMARIES)
}

/// Linear BT.2020 → BT.709 gamut matrix (the tone-map path's gamut
/// step). Matches ITU-R BT.2087 to within transcription tolerance.
pub fn bt2020_to_bt709_matrix() -> Mat3 {
    gamut_matrix(&BT2020_PRIMARIES, &BT709_PRIMARIES)
}

// ---------------------------------------------------------------------
// Transfer functions — both directions.
// ---------------------------------------------------------------------

const PQ_M1: f32 = 0.1593017578125; // 2610/16384
const PQ_M2: f32 = 78.84375; // 2523/32
const PQ_C1: f32 = 0.8359375; // 3424/4096
const PQ_C2: f32 = 18.8515625; // 2413/128
const PQ_C3: f32 = 18.6875; // 2392/128

/// SMPTE ST 2084 EOTF: PQ electrical signal `[0,1]` → absolute
/// luminance in cd/m². `pq_eotf(1.0) == 10000`.
pub fn pq_eotf(e_prime: f32) -> f32 {
    let e = e_prime.max(0.0);
    let e_p = e.powf(1.0 / PQ_M2);
    let num = (e_p - PQ_C1).max(0.0);
    let den = PQ_C2 - PQ_C3 * e_p;
    (num / den).powf(1.0 / PQ_M1) * PQ_MAX_NITS
}

/// Inverse SMPTE ST 2084 EOTF (a.k.a. the PQ OETF): absolute luminance
/// in cd/m² → PQ electrical signal `[0,1]`. This is the *encode*
/// direction — what the compositor uses to drive a PQ output.
pub fn pq_inverse_eotf(nits: f32) -> f32 {
    let y = (nits / PQ_MAX_NITS).clamp(0.0, 1.0);
    let ym = y.powf(PQ_M1);
    let num = PQ_C1 + PQ_C2 * ym;
    let den = 1.0 + PQ_C3 * ym;
    (num / den).powf(PQ_M2)
}

const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
const HLG_C: f32 = 0.55991073;

/// HLG OETF: scene-linear light `[0,1]` → HLG signal `[0,1]`. The
/// *encode* direction for an HLG output.
pub fn hlg_oetf(e: f32) -> f32 {
    let e = e.max(0.0);
    if e <= 1.0 / 12.0 {
        (3.0 * e).sqrt()
    } else {
        HLG_A * (12.0 * e - HLG_B).ln() + HLG_C
    }
}

/// Inverse HLG OETF: HLG signal `[0,1]` → scene-linear light `[0,1]`.
/// The *decode* direction (identical maths to
/// [`crate::shell::hdr_tonemap`]'s HLG decode, minus the display-peak
/// scale).
pub fn hlg_inverse_oetf(e_prime: f32) -> f32 {
    let e = e_prime.max(0.0);
    if e <= 0.5 {
        (e * e) / 3.0
    } else {
        (((e - HLG_C) / HLG_A).exp() + HLG_B) / 12.0
    }
}

/// sRGB EOTF: 8-bit-ish sRGB signal `[0,1]` → linear light `[0,1]`.
pub fn srgb_eotf(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB OETF: linear light `[0,1]` → sRGB signal `[0,1]`.
pub fn srgb_oetf(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

// ---------------------------------------------------------------------
// SDR → HDR mapping.
// ---------------------------------------------------------------------

/// Map an sRGB SDR pixel (linear-light BT.709, `[0,1]`) into
/// linear-light BT.2020 in absolute cd/m², placing SDR white at
/// [`SDR_REFERENCE_WHITE_NITS`]. This is the "SDR content is not
/// clipped, it is *placed* in the HDR volume" step.
pub fn map_sdr_linear_to_hdr_bt2020_nits(rgb_bt709_linear: [f32; 3]) -> [f32; 3] {
    let scaled = [
        rgb_bt709_linear[0] * SDR_REFERENCE_WHITE_NITS,
        rgb_bt709_linear[1] * SDR_REFERENCE_WHITE_NITS,
        rgb_bt709_linear[2] * SDR_REFERENCE_WHITE_NITS,
    ];
    bt709_to_bt2020_matrix().mul_vec(scaled)
}

/// Full SDR sRGB byte pixel → 10-bit PQ BT.2020 code values. This is
/// the CPU reference the [`OUTPUT_ENCODE_SHADER_SOURCE`] WGSL kernel
/// mirrors: `sRGB8 → linear709 → ×203 nits → BT.2020 → PQ → 10-bit`.
pub fn map_sdr_pixel_to_pq_bt2020(srgb8: [u8; 3]) -> [u16; 3] {
    let linear = [
        srgb_eotf(srgb8[0] as f32 / 255.0),
        srgb_eotf(srgb8[1] as f32 / 255.0),
        srgb_eotf(srgb8[2] as f32 / 255.0),
    ];
    let nits = map_sdr_linear_to_hdr_bt2020_nits(linear);
    [
        pq10_from_nits(nits[0]),
        pq10_from_nits(nits[1]),
        pq10_from_nits(nits[2]),
    ]
}

/// Absolute luminance (cd/m²) → 10-bit PQ code value `[0,1023]`.
pub fn pq10_from_nits(nits: f32) -> u16 {
    (pq_inverse_eotf(nits.max(0.0)) * 1023.0).round().clamp(0.0, 1023.0) as u16
}

// ---------------------------------------------------------------------
// Output colour state + per-surface composition decision.
// ---------------------------------------------------------------------

/// The colour space an output is being *scanned out* in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputColorSpace {
    /// Ordinary SDR: BT.709 primaries, sRGB/BT.1886 transfer. The
    /// compositor renders straight into an 8-bit sRGB swapchain.
    SdrSrgb,
    /// HDR10-style: BT.2020 primaries, PQ (ST 2084) transfer.
    HdrPq,
    /// HDR HLG: BT.2020 primaries, HLG transfer.
    HdrHlg,
}

impl OutputColorSpace {
    pub fn is_hdr(self) -> bool {
        matches!(self, OutputColorSpace::HdrPq | OutputColorSpace::HdrHlg)
    }

    /// The transfer characteristics an image description for this output
    /// should advertise to `color-management-v1` clients.
    pub fn transfer(self) -> TransferCharacteristics {
        match self {
            OutputColorSpace::SdrSrgb => TransferCharacteristics::Bt709,
            OutputColorSpace::HdrPq => TransferCharacteristics::SmpteSt2084,
            OutputColorSpace::HdrHlg => TransferCharacteristics::AribStdB67,
        }
    }

    /// The primaries an image description for this output should
    /// advertise.
    pub fn primaries(self) -> ColorPrimaries {
        match self {
            OutputColorSpace::SdrSrgb => ColorPrimaries::Bt709,
            OutputColorSpace::HdrPq | OutputColorSpace::HdrHlg => ColorPrimaries::Bt2020,
        }
    }
}

/// The resolved colour configuration for one output: the space it is
/// driven in plus the peak luminance the tone-mapper / SDR-mapper sizes
/// against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputColorState {
    pub space: OutputColorSpace,
    /// Panel peak luminance in cd/m². 100 for SDR, the EDID/probe peak
    /// for HDR.
    pub max_luminance_nits: f32,
}

impl OutputColorState {
    /// The safe default: an SDR sRGB desktop at 100 cd/m². Every
    /// uncertain path resolves here so we never scan PQ out to a panel
    /// that can't show it.
    pub const fn sdr_default() -> Self {
        Self {
            space: OutputColorSpace::SdrSrgb,
            max_luminance_nits: 100.0,
        }
    }

    /// Resolve the output colour state from a wgpu/EDID display probe.
    /// PQ is preferred over HLG when both are available (HDR10 is the
    /// desktop-content baseline); HLG is only chosen when the panel
    /// advertises HLG but not PQ.
    pub fn from_display(caps: &DisplayCapabilities) -> Self {
        if caps.supports_pq_output {
            Self {
                space: OutputColorSpace::HdrPq,
                max_luminance_nits: caps.max_luminance_nits.max(SDR_REFERENCE_WHITE_NITS),
            }
        } else if caps.supports_hlg_output {
            Self {
                space: OutputColorSpace::HdrHlg,
                max_luminance_nits: caps.max_luminance_nits.max(SDR_REFERENCE_WHITE_NITS),
            }
        } else {
            Self::sdr_default()
        }
    }

    /// The wgpu swapchain format this output should be configured with.
    /// `None` means "use the ordinary SDR 8-bit sRGB swapchain"; a
    /// `Some` is an HDR-carrying format the backend must configure the
    /// surface with (and then drive with PQ/HLG-encoded pixels).
    pub fn preferred_swapchain_format(&self) -> Option<vello::wgpu::TextureFormat> {
        match self.space {
            OutputColorSpace::SdrSrgb => None,
            OutputColorSpace::HdrPq | OutputColorSpace::HdrHlg => {
                if self.max_luminance_nits >= 800.0 {
                    Some(vello::wgpu::TextureFormat::Rgba16Float)
                } else {
                    Some(vello::wgpu::TextureFormat::Rgb10a2Unorm)
                }
            }
        }
    }
}

/// What the compositor must do to a surface's pixels to composite it
/// correctly onto a given output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SurfaceColorOp {
    /// The surface is already in the output's colour space — upload as
    /// is (the fast path, and the only path for SDR-on-SDR).
    Passthrough,
    /// HDR surface, SDR output: tone-map down. Carries the config so the
    /// caller sizes compression against the source peak.
    TonemapHdrToSdr { config: HdrTonemapConfig },
    /// SDR surface, HDR output: map into the HDR container at the
    /// reference white level and expand the gamut ([`map_sdr_pixel_to_pq_bt2020`]).
    MapSdrToHdr,
    /// HLG surface, PQ output (or vice versa): re-encode between the two
    /// HDR transfers, keeping BT.2020 primaries.
    TranscodeHdrTransfer,
}

/// Decide how to composite a surface with the given colorimetry onto an
/// output in the given colour state. This is the compositor's central
/// mixed-SDR/HDR composition decision.
///
/// `source_hdr_peak_nits` is the surface's mastering/content peak when
/// known (drives the tone-map target); pass `None` for the HDR10
/// 1000-nit default.
pub fn plan_surface_op(
    surface: Colorimetry,
    output: &OutputColorState,
    source_hdr_peak_nits: Option<f32>,
) -> SurfaceColorOp {
    let surface_hdr = surface.is_hdr();
    match output.space {
        OutputColorSpace::SdrSrgb => {
            if surface_hdr {
                let mut config = HdrTonemapConfig::default();
                if let Some(peak) = source_hdr_peak_nits {
                    if peak.is_finite() && peak > 0.0 {
                        config.source_peak_nits = peak;
                    }
                }
                config.target_peak_nits = output.max_luminance_nits;
                SurfaceColorOp::TonemapHdrToSdr { config }
            } else {
                SurfaceColorOp::Passthrough
            }
        }
        OutputColorSpace::HdrPq => {
            if !surface_hdr {
                SurfaceColorOp::MapSdrToHdr
            } else if surface.transfer == TransferCharacteristics::AribStdB67 {
                // HLG surface on a PQ output — transcode transfers.
                SurfaceColorOp::TranscodeHdrTransfer
            } else {
                SurfaceColorOp::Passthrough
            }
        }
        OutputColorSpace::HdrHlg => {
            if !surface_hdr {
                SurfaceColorOp::MapSdrToHdr
            } else if surface.transfer == TransferCharacteristics::SmpteSt2084 {
                // PQ surface on an HLG output — transcode transfers.
                SurfaceColorOp::TranscodeHdrTransfer
            } else {
                SurfaceColorOp::Passthrough
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::colorimetry::{ColorRange, MatrixCoefficients};

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    // --- Matrix algebra -------------------------------------------------

    #[test]
    fn mat3_inverse_round_trips_to_identity() {
        let m = bt709_to_bt2020_matrix();
        let inv = m.inverse().expect("invertible");
        let prod = m.mul(&inv);
        assert!(
            prod.max_abs_diff(&Mat3::IDENTITY) < 1e-4,
            "M·M⁻¹ should be identity, got {prod:?}"
        );
    }

    #[test]
    fn gamut_matrices_are_mutual_inverses() {
        let fwd = bt709_to_bt2020_matrix();
        let back = bt2020_to_bt709_matrix();
        let prod = fwd.mul(&back);
        assert!(
            prod.max_abs_diff(&Mat3::IDENTITY) < 1e-4,
            "709→2020→709 should round-trip, got {prod:?}"
        );
    }

    #[test]
    fn white_maps_to_white_across_gamuts() {
        // Equal-energy RGB white (1,1,1) in BT.709 is the D65 white
        // point; because both spaces share D65 it must stay (1,1,1)
        // (to rounding) in BT.2020 — the classic "white doesn't shift"
        // sanity check.
        let w = bt709_to_bt2020_matrix().mul_vec([1.0, 1.0, 1.0]);
        for (i, c) in w.iter().enumerate() {
            assert!(approx(*c, 1.0, 2e-3), "white channel {i} = {c}, expected 1.0");
        }
    }

    #[test]
    fn bt2020_to_bt709_matches_itu_reference() {
        // ITU-R BT.2087 / the widely-published linear BT.2020→709
        // matrix. Our derived matrix must match within transcription
        // tolerance (the published constants are rounded to 4dp).
        let reference = Mat3 {
            m: [
                [1.660491, -0.587641, -0.072850],
                [-0.124550, 1.132900, -0.008349],
                [-0.018151, -0.100579, 1.118730],
            ],
        };
        let derived = bt2020_to_bt709_matrix();
        assert!(
            derived.max_abs_diff(&reference) < 5e-3,
            "derived BT.2020→709 {derived:?} diverges from reference {reference:?}"
        );
    }

    #[test]
    fn bt709_to_bt2020_matches_itu_reference() {
        // The published forward matrix (Y'CbCr colour conversion notes).
        let reference = Mat3 {
            m: [
                [0.627404, 0.329283, 0.043313],
                [0.069097, 0.919540, 0.011362],
                [0.016391, 0.088013, 0.895595],
            ],
        };
        let derived = bt709_to_bt2020_matrix();
        assert!(
            derived.max_abs_diff(&reference) < 5e-3,
            "derived BT.709→2020 {derived:?} diverges from reference {reference:?}"
        );
    }

    #[test]
    fn rgb_to_xyz_rows_of_bt709_give_known_luminance() {
        // The middle (Y) row of the BT.709 RGB→XYZ matrix is the
        // luma-coefficient vector (0.2126, 0.7152, 0.0722).
        let m = rgb_to_xyz(&BT709_PRIMARIES);
        assert!(approx(m.m[1][0], 0.2126, 2e-3), "Yr = {}", m.m[1][0]);
        assert!(approx(m.m[1][1], 0.7152, 2e-3), "Yg = {}", m.m[1][1]);
        assert!(approx(m.m[1][2], 0.0722, 2e-3), "Yb = {}", m.m[1][2]);
    }

    // --- Transfer functions --------------------------------------------

    #[test]
    fn pq_eotf_reference_points() {
        assert!(pq_eotf(0.0) < 0.01, "PQ(0) ≈ 0");
        assert!(
            approx(pq_eotf(1.0), 10_000.0, 1.0),
            "PQ(1) = {}",
            pq_eotf(1.0)
        );
        // The canonical PQ mid-code: 0.5 electrical ≈ 92 cd/m².
        assert!(
            approx(pq_eotf(0.5), 92.0, 8.0),
            "PQ(0.5) = {}",
            pq_eotf(0.5)
        );
    }

    #[test]
    fn pq_forward_inverse_round_trip() {
        // Encode a spread of luminances and decode them back.
        for &nits in &[0.0, 0.1, 1.0, 10.0, 100.0, 203.0, 500.0, 1000.0, 4000.0, 10_000.0] {
            let e = pq_inverse_eotf(nits);
            assert!((0.0..=1.0).contains(&e), "PQ signal out of range for {nits}: {e}");
            let back = pq_eotf(e);
            // Relative tolerance — PQ is perceptual so absolute error
            // grows with luminance.
            let tol = (nits * 0.01).max(0.5);
            assert!(
                approx(back, nits, tol),
                "PQ round-trip {nits} → {e} → {back}"
            );
        }
    }

    #[test]
    fn pq_inverse_is_monotonic() {
        let mut prev = -1.0;
        let mut n = 0.0;
        while n <= 10_000.0 {
            let e = pq_inverse_eotf(n);
            assert!(e >= prev, "PQ inverse not monotonic at {n}: {e} < {prev}");
            prev = e;
            n += 50.0;
        }
    }

    #[test]
    fn hlg_oetf_inverse_round_trip() {
        for i in 0..=100 {
            let scene = i as f32 / 100.0;
            let signal = hlg_oetf(scene);
            assert!((0.0..=1.001).contains(&signal), "HLG signal range {signal}");
            let back = hlg_inverse_oetf(signal);
            assert!(
                approx(back, scene, 2e-3),
                "HLG round-trip {scene} → {signal} → {back}"
            );
        }
    }

    #[test]
    fn hlg_oetf_reference_points() {
        // HLG maps scene 0 → 0 and scene 1.0 → 1.0 (peak).
        assert!(approx(hlg_oetf(0.0), 0.0, 1e-4));
        assert!(approx(hlg_oetf(1.0), 1.0, 1e-4));
        // The knee is at scene 1/12 → signal 0.5.
        assert!(approx(hlg_oetf(1.0 / 12.0), 0.5, 1e-3), "HLG knee");
    }

    #[test]
    fn srgb_transfer_round_trips() {
        for i in 0..=255 {
            let v = i as f32 / 255.0;
            let linear = srgb_eotf(v);
            let back = srgb_oetf(linear);
            assert!(approx(back, v, 1e-3), "sRGB round-trip {v} → {linear} → {back}");
        }
    }

    // --- SDR → HDR mapping ---------------------------------------------

    #[test]
    fn sdr_white_maps_to_reference_white_nits() {
        // sRGB white (1,1,1) linear → BT.2020 nits should sit at ~203
        // cd/m² on every channel (D65 neutral).
        let nits = map_sdr_linear_to_hdr_bt2020_nits([1.0, 1.0, 1.0]);
        for (i, c) in nits.iter().enumerate() {
            assert!(
                approx(*c, SDR_REFERENCE_WHITE_NITS, 1.0),
                "SDR white channel {i} = {c} nits, expected {SDR_REFERENCE_WHITE_NITS}"
            );
        }
    }

    #[test]
    fn sdr_white_pq_code_round_trips_to_203_nits() {
        // The full byte→PQ path: sRGB white → PQ 10-bit code whose EOTF
        // is ~203 nits, and the channels stay neutral (grey).
        let code = map_sdr_pixel_to_pq_bt2020([255, 255, 255]);
        let nits: Vec<f32> = code.iter().map(|c| pq_eotf(*c as f32 / 1023.0)).collect();
        for (i, n) in nits.iter().enumerate() {
            assert!(
                approx(*n, SDR_REFERENCE_WHITE_NITS, 6.0),
                "white channel {i} decodes to {n} nits, expected ~203"
            );
        }
        // Neutral: R ≈ G ≈ B in code space.
        let max_delta = code.iter().copied().max().unwrap() as i32
            - code.iter().copied().min().unwrap() as i32;
        assert!(max_delta <= 3, "SDR white PQ codes should be neutral, got {code:?}");
    }

    #[test]
    fn sdr_black_maps_to_zero_pq() {
        let code = map_sdr_pixel_to_pq_bt2020([0, 0, 0]);
        assert_eq!(code, [0, 0, 0], "SDR black must encode to PQ 0");
    }

    #[test]
    fn sdr_map_expands_gamut_not_clips() {
        // Pure sRGB red should keep most of its luminance in the red
        // channel and gain a small positive contribution structure in
        // BT.2020 (never negative — that would be clipping).
        let nits = map_sdr_linear_to_hdr_bt2020_nits([1.0, 0.0, 0.0]);
        assert!(nits[0] > 0.0, "red channel should be positive: {nits:?}");
        // BT.709 red is inside BT.2020, so the mapped value has small
        // non-negative green/blue leakage from the matrix — crucially
        // the red primary is representable, so no channel is wildly
        // out of range.
        for c in nits {
            assert!(c >= -1e-3, "no channel should go negative (clip): {nits:?}");
        }
    }

    // --- Output colour state -------------------------------------------

    #[test]
    fn sdr_display_resolves_to_sdr_state() {
        let state = OutputColorState::from_display(&DisplayCapabilities::sdr_default());
        assert_eq!(state.space, OutputColorSpace::SdrSrgb);
        assert!(state.preferred_swapchain_format().is_none());
    }

    #[test]
    fn hdr10_display_resolves_to_pq_state_with_hdr_format() {
        let state = OutputColorState::from_display(&DisplayCapabilities::hdr10_baseline());
        assert_eq!(state.space, OutputColorSpace::HdrPq);
        assert!(state.max_luminance_nits >= 1000.0);
        assert!(
            state.preferred_swapchain_format().is_some(),
            "HDR output must select an HDR swapchain format"
        );
    }

    #[test]
    fn hlg_only_display_resolves_to_hlg() {
        let caps = DisplayCapabilities {
            max_luminance_nits: 600.0,
            supports_pq_output: false,
            supports_hlg_output: true,
            bt2020_gamut: true,
        };
        let state = OutputColorState::from_display(&caps);
        assert_eq!(state.space, OutputColorSpace::HdrHlg);
        assert_eq!(
            state.preferred_swapchain_format(),
            Some(vello::wgpu::TextureFormat::Rgb10a2Unorm),
            "600-nit HDR stays on 10-bit unorm, not float"
        );
    }

    // --- Composition decision (the four quadrants) ---------------------

    fn sdr() -> Colorimetry {
        Colorimetry::sdr_bt709()
    }
    fn hdr_pq() -> Colorimetry {
        Colorimetry::hdr10()
    }
    fn hdr_hlg() -> Colorimetry {
        Colorimetry::hdr_hlg()
    }

    #[test]
    fn sdr_surface_on_sdr_output_is_passthrough() {
        let out = OutputColorState::sdr_default();
        assert_eq!(plan_surface_op(sdr(), &out, None), SurfaceColorOp::Passthrough);
    }

    #[test]
    fn hdr_surface_on_sdr_output_tonemaps_with_peak() {
        let out = OutputColorState::sdr_default();
        match plan_surface_op(hdr_pq(), &out, Some(4000.0)) {
            SurfaceColorOp::TonemapHdrToSdr { config } => {
                assert!(approx(config.source_peak_nits, 4000.0, 0.1));
                assert!(approx(config.target_peak_nits, 100.0, 0.1));
            }
            other => panic!("expected tone-map, got {other:?}"),
        }
    }

    #[test]
    fn sdr_surface_on_hdr_output_maps_into_hdr() {
        let out = OutputColorState::from_display(&DisplayCapabilities::hdr10_baseline());
        assert_eq!(plan_surface_op(sdr(), &out, None), SurfaceColorOp::MapSdrToHdr);
    }

    #[test]
    fn hdr_pq_surface_on_hdr_pq_output_is_passthrough() {
        let out = OutputColorState::from_display(&DisplayCapabilities::hdr10_baseline());
        assert_eq!(plan_surface_op(hdr_pq(), &out, None), SurfaceColorOp::Passthrough);
    }

    #[test]
    fn hlg_surface_on_pq_output_transcodes() {
        let out = OutputColorState::from_display(&DisplayCapabilities::hdr10_baseline());
        assert_eq!(
            plan_surface_op(hdr_hlg(), &out, None),
            SurfaceColorOp::TranscodeHdrTransfer
        );
    }

    #[test]
    fn pq_surface_on_hlg_output_transcodes() {
        let out = OutputColorState {
            space: OutputColorSpace::HdrHlg,
            max_luminance_nits: 1000.0,
        };
        assert_eq!(
            plan_surface_op(hdr_pq(), &out, None),
            SurfaceColorOp::TranscodeHdrTransfer
        );
    }

    #[test]
    fn output_color_space_advertises_expected_cicp() {
        assert_eq!(
            OutputColorSpace::HdrPq.transfer(),
            TransferCharacteristics::SmpteSt2084
        );
        assert_eq!(OutputColorSpace::HdrPq.primaries(), ColorPrimaries::Bt2020);
        assert_eq!(
            OutputColorSpace::SdrSrgb.transfer(),
            TransferCharacteristics::Bt709
        );
        assert_eq!(OutputColorSpace::SdrSrgb.primaries(), ColorPrimaries::Bt709);
    }

    #[test]
    fn surface_op_ignores_matrix_and_range_fields() {
        // The decision keys on transfer only; a full-range HDR surface
        // still tone-maps on an SDR output.
        let full_range_hdr = Colorimetry {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferCharacteristics::SmpteSt2084,
            matrix: MatrixCoefficients::Bt2020NonConstant,
            range: ColorRange::Full,
        };
        let out = OutputColorState::sdr_default();
        assert!(matches!(
            plan_surface_op(full_range_hdr, &out, None),
            SurfaceColorOp::TonemapHdrToSdr { .. }
        ));
    }
}
