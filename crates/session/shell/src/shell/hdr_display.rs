//! Display capabilities for the receive-side `PeerView` and the output colour
//! pipeline: can this output show PQ pixels natively, and how bright?
//!
//! Built from the sink's EDID ([`crate::wayland::edid::HdrDisplayInfo`]) and
//! from whether the compositor is actually driving the output in HDR. A
//! presentation-surface probe cannot answer this: on DRM the compositor owns
//! the scanout buffers, and a format being renderable says nothing about the
//! sink accepting a PQ signal.
//!
//! A wrong negative only costs quality; a wrong positive sends PQ pixels to
//! an SDR panel and clips to black. Every uncertain path therefore returns
//! [`DisplayCapabilities::sdr_default`].

use crate::collab::colorimetry::{ColorPrimaries, TransferCharacteristics};
use crate::wayland::edid::HdrDisplayInfo;

/// What the compositor can deliver to one output. Consumed by
/// [`crate::shell::peer_view::PeerView::new_for_stream`] to pick between
/// tone-map and passthrough.
#[derive(Debug, Clone, Copy, PartialEq)]
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
        // Phase `an_sdr_sink_is_sdr_whatever_the_compositor_does`.
        assert_eq!(
            DisplayCapabilities::from_edid(&HdrDisplayInfo::default_sdr(), true),
            DisplayCapabilities::sdr_default()
        );
        // Phase `an_hdr_sink_driven_in_sdr_is_still_sdr`.
        assert_eq!(
            DisplayCapabilities::from_edid(&hdr_sink(1000), false),
            DisplayCapabilities::sdr_default()
        );
        // Phase `an_active_hdr_output_reports_its_edid_peak_and_gamut`.
        let active = DisplayCapabilities::from_edid(&hdr_sink(603), true);
        assert!(active.supports_pq_output);
        assert!(!active.supports_hlg_output, "Tilo signals PQ only");
        assert!(active.bt2020_gamut);
        assert!((active.max_luminance_nits - 603.0).abs() < f32::EPSILON);
        // Phase `an_unknown_peak_assumes_the_lowest_displayhdr_tier`.
        let unknown = DisplayCapabilities::from_edid(&hdr_sink(0), true);
        assert!(
            (unknown.max_luminance_nits - f32::from(UNKNOWN_HDR_PEAK_NITS)).abs() < f32::EPSILON
        );
    }
}
