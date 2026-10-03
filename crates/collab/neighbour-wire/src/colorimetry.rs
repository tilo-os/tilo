//! Colorimetry types — one central definition for primaries / transfer
//! / matrix used across capture, encode, wire, decode, display.
//!
//! We follow the H.273 / CICP (Coding-Independent Code Points)
//! numbering because that's what libavcodec + Wayland
//! `color-management-v1` both speak. If a codec / wire consumer needs a
//! different encoding (e.g. Y'`CbCr` matrix as a `MATRIX_COEFFICIENTS`
//! integer for HEVC VUI), it converts from these enums at the boundary
//! — the on-wire representation is documented on each variant.
//!
//! # References
//!
//! - ITU-T H.273 (2016) — CICP tables. `ColourPrimaries`,
//!   `TransferCharacteristics`, `MatrixCoefficients`.
//! - CTA-861.3 — HDR static metadata. Superseded by SMPTE ST 2086 for
//!   mastering-display info; both are shipped in the HDR10 SEI stream.
//! - Wayland `color-management-v1` (staging) — the same enums under
//!   different names (`WpColorManagerV1Primaries` etc.).

use serde::{Deserialize, Serialize};

/// Colour primaries — CIE 1931 (x, y) chromaticity coordinates for the
/// R/G/B/W points, encoded as a CICP `ColourPrimaries` integer.
///
/// The two we care about in practice:
/// - [`Self::Bt709`] — SDR displays, sRGB, "everything computer video"
///   (H.264 default, CICP 1).
/// - [`Self::Bt2020`] — HDR displays. Superset gamut required by
///   HDR10 / Dolby Vision (CICP 9).
///
/// Other CICP variants exist (SMPTE 170M, film P3, ...) but aren't
/// are unsupported until a hardware pipeline can render them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorPrimaries {
    /// BT.709 / sRGB primaries. CICP 1.
    Bt709,
    /// BT.2020 primaries — wide gamut required by HDR10. CICP 9.
    Bt2020,
    /// DCI-P3 / Display P3. CICP 12.
    DisplayP3,
    /// Unspecified — decoder infers from stream defaults. CICP 2.
    Unspecified,
}

impl ColorPrimaries {
    /// CICP integer per H.273 Table 2.
    pub fn cicp(self) -> u8 {
        match self {
            ColorPrimaries::Bt709 => 1,
            ColorPrimaries::Unspecified => 2,
            ColorPrimaries::Bt2020 => 9,
            ColorPrimaries::DisplayP3 => 12,
        }
    }
}

/// Transfer characteristics — the electro-optical transfer function
/// (EOTF) that turns codec-domain samples into linear light on the
/// display. CICP `TransferCharacteristics`.
///
/// - [`Self::Bt709`] — SDR gamma-2.2-ish. CICP 1.
/// - [`Self::SmpteSt2084`] — PQ (Perceptual Quantizer), absolute
///   0..10000 cd/m² mapping. HDR10's transfer. CICP 16.
/// - [`Self::AribStdB67`] — HLG (Hybrid Log-Gamma), relative
///   scene-referred. HDR variant for broadcast. CICP 18.
///
/// PQ and HLG are the two "HDR transfers"; when we say "HDR" without
/// qualifying we mean [`Self::SmpteSt2084`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransferCharacteristics {
    /// BT.709 / sRGB SDR transfer. CICP 1.
    Bt709,
    /// SMPTE ST 2084 / PQ — HDR10's absolute-luminance EOTF. CICP 16.
    SmpteSt2084,
    /// ARIB STD-B67 / HLG — HDR relative-scene-referred EOTF. CICP 18.
    AribStdB67,
    /// sRGB piecewise-linear (matches sRGB spec, subtly different from
    /// BT.709). CICP 13.
    Srgb,
    /// Linear light, extended range: `1.0` is SDR reference white and
    /// values above it are HDR highlights (scRGB with BT.709 primaries).
    /// CICP 8.
    Linear,
    /// Unspecified. CICP 2.
    Unspecified,
}

impl TransferCharacteristics {
    pub fn cicp(self) -> u8 {
        match self {
            TransferCharacteristics::Bt709 => 1,
            TransferCharacteristics::Unspecified => 2,
            TransferCharacteristics::Linear => 8,
            TransferCharacteristics::Srgb => 13,
            TransferCharacteristics::SmpteSt2084 => 16,
            TransferCharacteristics::AribStdB67 => 18,
        }
    }

    /// Is this an HDR EOTF? Callers use it to pick between the SDR
    /// tone-map path and the direct HDR upload path.
    pub fn is_hdr(self) -> bool {
        matches!(
            self,
            TransferCharacteristics::SmpteSt2084 | TransferCharacteristics::AribStdB67
        )
    }
}

/// Y'`CbCr` matrix — the RGB↔Y'`CbCr` conversion matrix used by the
/// codec's chroma subsampling. CICP `MatrixCoefficients`.
///
/// - [`Self::Bt709`] — matches BT.709 primaries; SDR video. CICP 1.
/// - [`Self::Bt2020NonConstant`] — non-constant luminance BT.2020
///   for HDR10. CICP 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatrixCoefficients {
    /// BT.709 matrix. CICP 1.
    Bt709,
    /// BT.2020 non-constant luminance. CICP 9.
    Bt2020NonConstant,
    /// Identity (RGB) — used for RGB-in-codec (H.264 High 4:4:4). CICP 0.
    Identity,
    /// Unspecified. CICP 2.
    Unspecified,
}

impl MatrixCoefficients {
    pub fn cicp(self) -> u8 {
        match self {
            MatrixCoefficients::Identity => 0,
            MatrixCoefficients::Bt709 => 1,
            MatrixCoefficients::Unspecified => 2,
            MatrixCoefficients::Bt2020NonConstant => 9,
        }
    }
}

/// Range of the quantised luma samples in the codec bitstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorRange {
    /// TV / "limited" range — luma 16..=235 for 8-bit, 64..=940 for 10-bit.
    Limited,
    /// PC / "full" range — luma 0..=255 for 8-bit, 0..=1023 for 10-bit.
    Full,
}

/// Bundle of every colorimetry field the encoder + decoder + display
/// need to agree on. Sits inside [`crate::NeighbourMsg::SurfaceStreamStart`]
/// so late-joining peers know how to interpret pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Colorimetry {
    pub primaries: ColorPrimaries,
    pub transfer: TransferCharacteristics,
    pub matrix: MatrixCoefficients,
    pub range: ColorRange,
}

impl Colorimetry {
    /// Standard SDR — BT.709 primaries + BT.709 transfer + BT.709
    /// matrix + limited range. What every openh264 stream defaults to.
    pub const fn sdr_bt709() -> Self {
        Colorimetry {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferCharacteristics::Bt709,
            matrix: MatrixCoefficients::Bt709,
            range: ColorRange::Limited,
        }
    }

    /// HDR10 — BT.2020 primaries + PQ transfer + BT.2020 non-constant
    /// matrix + limited range.
    pub const fn hdr10() -> Self {
        Colorimetry {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferCharacteristics::SmpteSt2084,
            matrix: MatrixCoefficients::Bt2020NonConstant,
            range: ColorRange::Limited,
        }
    }

    /// HDR HLG — BT.2020 primaries + HLG transfer + BT.2020
    /// non-constant matrix. Broadcast HDR flavour.
    pub const fn hdr_hlg() -> Self {
        Colorimetry {
            primaries: ColorPrimaries::Bt2020,
            transfer: TransferCharacteristics::AribStdB67,
            matrix: MatrixCoefficients::Bt2020NonConstant,
            range: ColorRange::Limited,
        }
    }

    /// Convenience — is the transfer function HDR?
    pub fn is_hdr(self) -> bool {
        self.transfer.is_hdr()
    }
}

impl Default for Colorimetry {
    fn default() -> Self {
        Self::sdr_bt709()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CICP code points match H.273, HDR is detected, and colorimetry serialises kebab-case.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn colorimetry_maps_to_h273_detects_hdr_and_serialises_kebab_case() {
        cicp_matches_h273();
        hdr_detection();
        colorimetry_serialises_kebab_case();
    }

    fn cicp_matches_h273() {
        // Table 2 spot checks.
        assert_eq!(ColorPrimaries::Bt709.cicp(), 1);
        assert_eq!(ColorPrimaries::Bt2020.cicp(), 9);
        assert_eq!(TransferCharacteristics::SmpteSt2084.cicp(), 16);
        assert_eq!(TransferCharacteristics::AribStdB67.cicp(), 18);
        assert_eq!(MatrixCoefficients::Bt2020NonConstant.cicp(), 9);
    }

    fn hdr_detection() {
        assert!(!Colorimetry::sdr_bt709().is_hdr());
        assert!(Colorimetry::hdr10().is_hdr());
        assert!(Colorimetry::hdr_hlg().is_hdr());
    }

    fn colorimetry_serialises_kebab_case() {
        let s = serde_json::to_string(&Colorimetry::hdr10()).unwrap();
        assert!(s.contains("bt2020"), "got {s}");
        assert!(s.contains("smpte-st2084"), "got {s}");
    }
}
