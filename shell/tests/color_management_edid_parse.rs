//! CTA-861-G HDR display metadata parsing.
//!
//! Feeds a hand-constructed EDID blob into
//! [`shell::wayland::edid::parse_hdr_display_info`] and asserts that
//! the extracted BT.2020 primaries and 1000-nit peak luminance match
//! the values encoded into the blob.
//!
//! The blob mirrors a Dell U2720QM / LG C1 HDR display's advertised
//! CTA-861 extension block layout — BT.2020 primaries in the base
//! block, PQ + HLG in the HDR Static Metadata extended-tag block, and
//! a Colorimetry extended-tag block flagging BT.2020 support.

use shell::collab::colorimetry::{ColorPrimaries, TransferCharacteristics};
use shell::wayland::edid::{HdrDisplayInfo, parse_hdr_display_info};

fn hdr_edid_blob() -> [u8; 256] {
    let mut edid = [0u8; 256];
    edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    // Manufacturer id "DEL".
    edid[8] = 0x10;
    edid[9] = 0xAC;
    edid[18] = 1;
    edid[19] = 4;

    fn split(v: u16) -> (u8, u8) {
        let hi = ((v >> 2) & 0xFF) as u8;
        let lo = (v & 0x3) as u8;
        (lo, hi)
    }
    // BT.2020: R (0.708, 0.292), G (0.170, 0.797), B (0.131, 0.046).
    // D65 white (0.3127, 0.329).
    let (rx_lo, rx_hi) = split(725);
    let (ry_lo, ry_hi) = split(299);
    let (gx_lo, gx_hi) = split(174);
    let (gy_lo, gy_hi) = split(816);
    let (bx_lo, bx_hi) = split(134);
    let (by_lo, by_hi) = split(47);
    let (wx_lo, wx_hi) = split(320);
    let (wy_lo, wy_hi) = split(337);
    edid[25] = (rx_lo << 6) | (ry_lo << 4) | (gx_lo << 2) | gy_lo;
    edid[26] = (bx_lo << 6) | (by_lo << 4) | (wx_lo << 2) | wy_lo;
    edid[27] = rx_hi;
    edid[28] = ry_hi;
    edid[29] = gx_hi;
    edid[30] = gy_hi;
    edid[31] = bx_hi;
    edid[32] = by_hi;
    edid[33] = wx_hi;
    edid[34] = wy_hi;
    edid[126] = 1;

    // CTA-861 extension block: tag + revision + DTD offset just past
    // our two data blocks, then Colorimetry + HDR Static Metadata.
    edid[128] = 0x02;
    edid[129] = 3;
    edid[130] = 15;
    edid[131] = 0xE3;
    edid[132] = 0x05;
    edid[133] = 0b1110_0000;
    edid[134] = 0x00;
    edid[135] = 0xE6;
    edid[136] = 0x06;
    edid[137] = 0b0000_1101; // SDR + PQ + HLG
    edid[138] = 0x01;
    edid[139] = 138; // max ≈ 1000 cd/m²
    edid[140] = 96;  // FALL ≈ 400 cd/m²
    edid[141] = 18;  // min ≈ 0.05 cd/m²
    edid
}

#[test]
fn hdr_display_edid_extracts_bt2020_primaries() {
    let edid = hdr_edid_blob();
    let info: HdrDisplayInfo = parse_hdr_display_info(&edid);

    // Red primary ≈ (0.708, 0.292) → (708_000, 292_000) micro-CIE.
    let (rx, ry) = info.display_primaries[0];
    assert!(
        (rx as i64 - 708_000).abs() < 3_000,
        "red x ≈ 708_000, got {rx}"
    );
    assert!(
        (ry as i64 - 292_000).abs() < 3_000,
        "red y ≈ 292_000, got {ry}"
    );
    assert_eq!(info.best_named_primaries, ColorPrimaries::Bt2020);
}

#[test]
fn hdr_display_edid_extracts_peak_luminance() {
    let edid = hdr_edid_blob();
    let info = parse_hdr_display_info(&edid);
    assert!(
        (info.max_luminance_cd_m2 as i32 - 1000).abs() < 30,
        "expected ≈ 1000-nit peak, got {}",
        info.max_luminance_cd_m2
    );
    assert!(
        (info.max_frame_average_luminance_cd_m2 as i32 - 400).abs() < 20,
        "expected ≈ 400-nit FALL, got {}",
        info.max_frame_average_luminance_cd_m2
    );
}

#[test]
fn hdr_display_edid_advertises_pq_and_hlg() {
    let edid = hdr_edid_blob();
    let info = parse_hdr_display_info(&edid);
    assert!(info
        .supported_transfers
        .contains(&TransferCharacteristics::SmpteSt2084));
    assert!(info
        .supported_transfers
        .contains(&TransferCharacteristics::AribStdB67));
}

#[test]
fn empty_edid_returns_sdr_default() {
    let info = parse_hdr_display_info(&[]);
    assert_eq!(info.best_named_primaries, ColorPrimaries::Bt709);
    assert_eq!(info.max_luminance_cd_m2, 100);
}
