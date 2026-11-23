//! Display capabilities for the receive-side `PeerView` and the output colour
//! pipeline: can this output show PQ pixels natively, and how bright?
//!
//! Built from the sink's EDID ([`HdrDisplayInfo`]) and from whether the
//! compositor is actually driving the output in HDR. A
//! presentation-surface probe cannot answer this: on DRM the compositor owns
//! the scanout buffers, and a format being renderable says nothing about the
//! sink accepting a PQ signal.
//!
//! A wrong negative only costs quality; a wrong positive sends PQ pixels to
//! an SDR panel and clips to black. Every uncertain path therefore returns
//! [`DisplayCapabilities::sdr_default`].

use neighbour_wire::colorimetry::{ColorPrimaries, TransferCharacteristics};
use serde::{Deserialize, Serialize};

/// The bits of a display's EDID that matter for color-management-v1.
///
/// Chromaticity fields are CIE 1931 xy coordinates scaled the same
/// way the Wayland protocol reports them: (x, y) multiplied by
/// `1_000_000`. So (0.708, 0.292) — BT.2020 red — becomes
/// (`708_000`, `292_000`).
///
/// The EDID parser that fills this in reads the bytes a connector reports
/// (`tilo_wayland_ext::edid::parse_hdr_display_info`); the type lives here so
/// everything that only reads a display's colour needs no Wayland stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdrDisplayInfo {
    /// R, G, B primary chromaticities in (`x_micro`, `y_micro`).
    pub display_primaries: [(u32, u32); 3],
    /// White-point chromaticity in (`x_micro`, `y_micro`).
    pub white_point: (u32, u32),
    /// Desired content peak luminance in cd/m². `0` = unknown.
    pub max_luminance_cd_m2: u16,
    /// Desired content frame-average peak luminance in cd/m². `0` = unknown.
    pub max_frame_average_luminance_cd_m2: u16,
    /// Desired content minimum luminance in cd/m² * 10000 (i.e. the
    /// same 1/10000 unit the SMPTE ST 2086 metadata payload uses).
    /// `0` = unknown.
    pub min_luminance_micro_cd_m2: u32,
    /// Which EOTFs the display accepts. Reduced to the small set we
    /// route into `color-management-v1`.
    pub supported_transfers: Vec<TransferCharacteristics>,
    /// The best-guess named gamut of the display, derived from the
    /// Colorimetry Data Block (if present) plus the base-block
    /// chromaticities.
    pub best_named_primaries: ColorPrimaries,
}

impl HdrDisplayInfo {
    /// SDR fallback for parseless paths (winit backend, headless
    /// tests). BT.709 primaries, 100-nit desktop reference, only the
    /// BT.1886 SDR EOTF advertised.
    pub fn default_sdr() -> Self {
        Self {
            // BT.709 R (0.640, 0.330), G (0.300, 0.600), B (0.150, 0.060).
            display_primaries: [(640_000, 330_000), (300_000, 600_000), (150_000, 60_000)],
            // D65 (0.3127, 0.329).
            white_point: (312_700, 329_000),
            max_luminance_cd_m2: 100,
            max_frame_average_luminance_cd_m2: 100,
            // 0.05 cd/m² typical.
            min_luminance_micro_cd_m2: 500,
            supported_transfers: vec![TransferCharacteristics::Bt709],
            best_named_primaries: ColorPrimaries::Bt709,
        }
    }
}

/// What the compositor can deliver to one output. Consumed by
/// the shell's peer view (`PeerView::new_for_stream`) to pick between
/// tone-map and passthrough.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct DisplayCapabilities {
    /// Peak sustainable luminance in cd/m² (SDR 100, HDR400 400, HDR1000
    /// 1000+); sets the tone-map target.
    pub max_luminance_nits: f32,
    /// The output is driven with a PQ (SMPTE ST 2084) signal right now:
    /// true → passthrough, false → tone-map to SDR.
    pub supports_pq_output: bool,
    /// The compositor drives the output with HLG. Tilo signals PQ only, so
    /// this stays false until an HLG output mode exists.
    pub supports_hlg_output: bool,
    /// Panel approximately covers BT.2020; when false, gamut-mapping still
    /// runs in the HDR path to compress toward the display's real gamut.
    pub bt2020_gamut: bool,
}

/// Peak assumed for an HDR sink whose EDID leaves its luminance unknown: the
/// lowest VESA `DisplayHDR` tier, so highlights are compressed rather than
/// clipped on the dimmest panel that can report PQ.
const UNKNOWN_HDR_PEAK_NITS: u16 = 400;

impl DisplayCapabilities {
    /// Conservative SDR default, returned on any probe failure.
    pub const fn sdr_default() -> Self {
        Self {
            max_luminance_nits: 100.0,
            supports_pq_output: false,
            supports_hlg_output: false,
            bt2020_gamut: false,
        }
    }

    /// HDR10 baseline — 1000 cd/m², PQ, BT.2020.
    pub const fn hdr10_baseline() -> Self {
        Self {
            max_luminance_nits: 1000.0,
            supports_pq_output: true,
            supports_hlg_output: false,
            bt2020_gamut: true,
        }
    }

    /// Capabilities of an output from its EDID and the compositor's signal.
    /// PQ is claimed only while `hdr_active`: the sink accepting PQ is not
    /// enough, the scanout must actually be sending it.
    pub fn from_edid(info: &HdrDisplayInfo, hdr_active: bool) -> Self {
        let sink_pq = info
            .supported_transfers
            .contains(&TransferCharacteristics::SmpteSt2084);
        if !(sink_pq && hdr_active) {
            return Self::sdr_default();
        }
        let max_luminance_nits = if info.max_luminance_cd_m2 > 0 {
            f32::from(info.max_luminance_cd_m2)
        } else {
            f32::from(UNKNOWN_HDR_PEAK_NITS)
        };
        Self {
            max_luminance_nits,
            supports_pq_output: true,
            supports_hlg_output: false,
            bt2020_gamut: info.best_named_primaries == ColorPrimaries::Bt2020,
        }
    }
}

impl Default for DisplayCapabilities {
    fn default() -> Self {
        Self::sdr_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr_sink(max_luminance_cd_m2: u16) -> HdrDisplayInfo {
        HdrDisplayInfo {
            max_luminance_cd_m2,
            supported_transfers: vec![
                TransferCharacteristics::Bt709,
                TransferCharacteristics::SmpteSt2084,
            ],
            best_named_primaries: ColorPrimaries::Bt2020,
            ..HdrDisplayInfo::default_sdr()
        }
    }

    #[test]
    fn capabilities_follow_the_edid_and_the_active_signal() {
        assert_eq!(
            DisplayCapabilities::from_edid(&HdrDisplayInfo::default_sdr(), true),
            DisplayCapabilities::sdr_default()
        );
        assert_eq!(
            DisplayCapabilities::from_edid(&hdr_sink(1000), false),
            DisplayCapabilities::sdr_default()
        );
        let active = DisplayCapabilities::from_edid(&hdr_sink(603), true);
        assert!(active.supports_pq_output);
        assert!(!active.supports_hlg_output, "Tilo signals PQ only");
        assert!(active.bt2020_gamut);
        assert!((active.max_luminance_nits - 603.0).abs() < f32::EPSILON);
        let unknown = DisplayCapabilities::from_edid(&hdr_sink(0), true);
        assert!(
            (unknown.max_luminance_nits - f32::from(UNKNOWN_HDR_PEAK_NITS)).abs() < f32::EPSILON
        );
    }
}
