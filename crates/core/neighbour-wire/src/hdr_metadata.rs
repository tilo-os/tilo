//! HDR10 static metadata — the two SEI messages every HDR bitstream
//! carries so the receiver can display in the sender's absolute
//! luminance intent.
//!
//! - **SMPTE ST 2086 mastering display colour volume** — the primary
//!   chromaticities and reference white point of the *mastering
//!   display*, plus the min/max luminance the content was authored
//!   against.
//! - **CTA-861.3 content light level** — `MaxCLL` (peak pixel over the
//!   entire program) and `MaxFALL` (peak frame-average over the
//!   entire program). Lets the display's tone mapper size its
//!   compression to the actual content, not the container's worst case.
//!
//! Both live in-band as SEI messages (HEVC / H.264) or in libavutil
//! frame side data. The types below are the semantic representation;
//! encoder backends convert to libavcodec's exact struct layouts.

use serde::{Deserialize, Serialize};

/// SMPTE ST 2086 mastering-display colour volume.
///
/// Uses the same integer scaling libavutil uses so backends can `memcpy`
/// straight into an `AVMasteringDisplayMetadata` — chromaticities as
/// 16-bit fractional (`x = display_primaries_x / 50000`), luminance in
/// 1/10000 cd/m² units.
///
/// Wire representation is `#[serde(default)]` so peers on the old
/// codec-string schema decode a null field, not a mangled value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MasteringDisplayColorVolume {
    /// Display primaries — chromaticity (x, y) as `1/50000` fractions
    /// (H.265 spec § D.2.28 encoding). Order is R, G, B.
    ///
    /// e.g. BT.709 D65: `display_primaries[0] = (32000, 16500)` (red x=0.64, y=0.33).
    pub display_primaries: [(u16, u16); 3],
    /// White point chromaticity (x, y) as `1/50000` fractions. D65 is
    /// (`15635`, `16450`) — the standard reference white.
    pub white_point: (u16, u16),
    /// Maximum display mastering luminance in `1/10000 cd/m²`. A common
    /// mastering display targets 1000 cd/m² → `10_000_000`. HDR10 upper
    /// bound is 10 000 cd/m² → `100_000_000`.
    pub max_display_mastering_luminance: u32,
    /// Minimum display mastering luminance in `1/10000 cd/m²`. Typical:
    /// 0.05 cd/m² → `500`.
    pub min_display_mastering_luminance: u32,
}

impl MasteringDisplayColorVolume {
    /// BT.2020 primaries + D65 white + 0..1000 cd/m² mastering.
    /// Mirrors the reference config every HDR10 display supports.
    pub const fn hdr10_reference() -> Self {
        Self {
            // BT.2020 R (0.708, 0.292), G (0.170, 0.797), B (0.131, 0.046).
            display_primaries: [(35400, 14600), (8500, 39850), (6550, 2300)],
            // D65 (0.3127, 0.329).
            white_point: (15635, 16450),
            // 1000 cd/m² max mastering, 0.05 cd/m² min.
            max_display_mastering_luminance: 10_000_000,
            min_display_mastering_luminance: 500,
        }
    }
}

/// CTA-861.3 content light level information.
///
/// Both fields in cd/m² directly (not fractional — the SEI encodes as
/// u16 nits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentLightLevel {
    /// `MaxCLL` — peak single-pixel luminance over the entire program.
    /// `0` = unknown, decoders skip.
    pub max_content_light_level: u16,
    /// `MaxFALL` — peak frame-average luminance over the entire program.
    /// `0` = unknown.
    pub max_frame_average_light_level: u16,
}

impl ContentLightLevel {
    /// Placeholder for capture-side pipelines that haven't measured the
    /// content — signals "unknown" to the decoder.
    pub const fn unknown() -> Self {
        Self {
            max_content_light_level: 0,
            max_frame_average_light_level: 0,
        }
    }
}

/// Attached to the wire as an optional field of
/// [`crate::NeighbourMsg::SurfaceStreamStart`]; SEI messages
/// carrying the same data are inserted in-band by the encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hdr10Metadata {
    pub mastering: MasteringDisplayColorVolume,
    pub light: ContentLightLevel,
}

impl Hdr10Metadata {
    /// Sensible default for pipelines that don't measure their content:
    /// 1000-nit BT.2020 reference mastering, unknown content light.
    pub const fn reference_bt2020() -> Self {
        Self {
            mastering: MasteringDisplayColorVolume::hdr10_reference(),
            light: ContentLightLevel::unknown(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr10_reference_uses_bt2020_primaries_at_expected_precision() {
        let m = MasteringDisplayColorVolume::hdr10_reference();
        // BT.2020 red primary is (0.708, 0.292). Encoded as
        // (round(0.708 * 50000), round(0.292 * 50000)) = (35400, 14600).
        assert_eq!(m.display_primaries[0], (35400, 14600));
        // D65 white: (0.3127, 0.329).
        assert_eq!(m.white_point, (15635, 16450));
        assert_eq!(m.max_display_mastering_luminance, 10_000_000);
    }
}
