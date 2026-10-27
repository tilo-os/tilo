//! HDR pipeline configuration.
//!
//! The HDR10 static metadata that crosses the wire lives in
//! [`neighbour_wire::hdr_metadata`] and is re-exported here; this module
//! adds the compositor-side knob that selects an HDR transfer and the
//! metadata to embed.

use serde::{Deserialize, Serialize};

pub use neighbour_wire::hdr_metadata::{
    ContentLightLevel, Hdr10Metadata, MasteringDisplayColorVolume,
};

/// HDR configuration — passed through [`crate::config::FlatpakShareConfig`]
/// and picked up by encoder + wire + decoder. `None` = SDR pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HdrConfig {
    /// PQ vs HLG. Selects the transfer function on both encoder and
    /// receiver; the primaries + matrix follow BT.2020 in either case.
    pub transfer: HdrTransfer,
    /// Static metadata to embed. Overriding this lets a workspace hint
    /// at its actual mastering monitor (e.g. a colourist's HDR reference
    /// display) — a downstream tone-mapper can then compress correctly.
    pub metadata: Hdr10Metadata,
}

/// Which HDR transfer function to use. Kept separate from
/// [`crate::collab::colorimetry::TransferCharacteristics`] because the
/// config knob only has two live values — PQ (HDR10 / HDR10+) or HLG
/// (broadcast).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HdrTransfer {
    /// SMPTE ST 2084 / PQ — HDR10 default. Absolute-luminance mapping,
    /// 0..10000 cd/m².
    Pq,
    /// ARIB STD-B67 / HLG — scene-referred relative mapping. Broadcast.
    Hlg,
}

impl HdrConfig {
    /// HDR10 PQ at 1000-nit BT.2020 reference — the "just turn on HDR"
    /// default. Individual workspaces override the metadata to match
    /// their actual mastering monitor.
    pub const fn pq_reference() -> Self {
        Self {
            transfer: HdrTransfer::Pq,
            metadata: Hdr10Metadata::reference_bt2020(),
        }
    }

    /// HLG variant, same metadata.
    pub const fn hlg_reference() -> Self {
        Self {
            transfer: HdrTransfer::Hlg,
            metadata: Hdr10Metadata::reference_bt2020(),
        }
    }

    /// Emit the receive-side [`crate::collab::colorimetry::Colorimetry`]
    /// that pairs with this HDR config. Always BT.2020 primaries + the
    /// matching transfer + BT.2020 non-constant matrix + limited range.
    pub fn colorimetry(self) -> crate::collab::colorimetry::Colorimetry {
        use crate::collab::colorimetry::{
            ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
        };
        Colorimetry {
            primaries: ColorPrimaries::Bt2020,
            transfer: match self.transfer {
                HdrTransfer::Pq => TransferCharacteristics::SmpteSt2084,
                HdrTransfer::Hlg => TransferCharacteristics::AribStdB67,
            },
            matrix: MatrixCoefficients::Bt2020NonConstant,
            range: ColorRange::Limited,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_metadata_pairs_colorimetry_and_round_trips() {
        {
            use crate::collab::colorimetry::TransferCharacteristics;
            let pq = HdrConfig::pq_reference();
            assert_eq!(
                pq.colorimetry().transfer,
                TransferCharacteristics::SmpteSt2084
            );
            let hlg = HdrConfig::hlg_reference();
            assert_eq!(
                hlg.colorimetry().transfer,
                TransferCharacteristics::AribStdB67
            );
        }
        {
            let cfg = HdrConfig::pq_reference();
            let s = serde_json::to_string(&cfg).unwrap();
            let back: HdrConfig = serde_json::from_str(&s).unwrap();
            assert_eq!(cfg, back);
        }
    }
}
