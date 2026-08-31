//! Minimal EDID / CTA-861-G parser for the fields we surface to
//! `color-management-v1` clients. Base block per VESA E-EDID 1.4;
//! extension block per CTA-861-G ("A DTV Profile for Uncompressed High
//! Speed Digital Interfaces").
//!
//! An EDID blob is 128 bytes of base information plus zero or more
//! 128-byte extension blocks. HDR-capable displays advertise their
//! peak/average/minimum luminance and preferred EOTFs (HLG, ST-2084 PQ,
//! gamma) through a CTA-861 extension block's HDR Static Metadata Data
//! Block (§7.5.13, `extended_tag = 6`) and Colorimetry Data Block
//! (§7.5.5, `extended_tag = 5`). We also read the base block's
//! chromaticity bytes so the compositor reports the display's actual
//! gamut rather than assuming BT.2020 for every HDR display.
//!
//! Not parsed: Detailed Timing Descriptors, monitor names, timings,
//! `DisplayID`, or the EDID checksum (the parser is defensive against
//! short/malformed blobs and returns `HdrDisplayInfo::default_sdr()`
//! rather than panicking, so a bogus checksum just falls back to SDR).

use crate::collab::colorimetry::{ColorPrimaries, TransferCharacteristics};

/// The bits of the EDID that matter for color-management-v1.
///
/// Chromaticity fields are CIE 1931 xy coordinates scaled the same
/// way the Wayland protocol reports them: (x, y) multiplied by
/// `1_000_000`. So (0.708, 0.292) — BT.2020 red — becomes
/// (`708_000`, `292_000`).
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

/// Parse an EDID byte blob and pull out the HDR display info.
///
/// Robust against short / garbage input — returns
/// [`HdrDisplayInfo::default_sdr`] when the blob is too small or the
/// CTA-861 extension block doesn't contain an HDR Static Metadata
/// data block.
#[allow(clippy::similar_names, reason = "R/G/B/W xy component names come from the EDID 1.4 chromaticity layout")]
pub fn parse_hdr_display_info(edid: &[u8]) -> HdrDisplayInfo {
    let mut info = HdrDisplayInfo::default_sdr();
    // A fixed-size view, so every constant index below is in bounds by type
    // rather than by a length check the reader has to keep in mind. EDID comes
    // off the wire from the display, so a short or hostile blob must not panic.
    let Some(base) = edid.get(..128).and_then(|b| <&[u8; 128]>::try_from(b).ok()) else {
        return info;
    };

    // ---- Base-block chromaticities ---------------------------------
    //
    // EDID 1.4 §3.7. Bytes 25..=34 pack Rx/Ry/Gx/Gy/Bx/By/Wx/Wy as
    // 10-bit fixed-point (0..=1023 = 0.0..≈1.0). The low 2 bits of
    // each 10-bit value live in a shared byte (25 for R+G, 26 for
    // B+W); the high 8 bits are bytes 27..=34.
    let lo_rg = base[25];
    let lo_bw = base[26];
    let hi_rx = u16::from(base[27]);
    let hi_ry = u16::from(base[28]);
    let hi_gx = u16::from(base[29]);
    let hi_gy = u16::from(base[30]);
    let hi_bx = u16::from(base[31]);
    let hi_by = u16::from(base[32]);
    let hi_wx = u16::from(base[33]);
    let hi_wy = u16::from(base[34]);

    let rx = (hi_rx << 2) | ((u16::from(lo_rg) >> 6) & 0b11);
    let ry = (hi_ry << 2) | ((u16::from(lo_rg) >> 4) & 0b11);
    let gx = (hi_gx << 2) | ((u16::from(lo_rg) >> 2) & 0b11);
    let gy = (hi_gy << 2) | (u16::from(lo_rg) & 0b11);
    let bx = (hi_bx << 2) | ((u16::from(lo_bw) >> 6) & 0b11);
    let by = (hi_by << 2) | ((u16::from(lo_bw) >> 4) & 0b11);
    let wx = (hi_wx << 2) | ((u16::from(lo_bw) >> 2) & 0b11);
    let wy = (hi_wy << 2) | (u16::from(lo_bw) & 0b11);

    // 10-bit fixed-point → x-micro (× 1e6). Value / 1024 is the raw
    // fractional coordinate; multiplied by 1_000_000 gives micro-CIE.
    let to_micro = |v: u16| -> u32 {
        // Do the divide in u64 to avoid intermediate overflow.
        ((u64::from(v) * 1_000_000) / 1024) as u32
    };
    info.display_primaries = [
        (to_micro(rx), to_micro(ry)),
        (to_micro(gx), to_micro(gy)),
        (to_micro(bx), to_micro(by)),
    ];
    info.white_point = (to_micro(wx), to_micro(wy));

    // Whether the chromaticities look like BT.2020 (wide-gamut HDR)
    // vs BT.709 (sRGB). Rough classifier — BT.2020 red is at ≈
    // (0.708, 0.292); BT.709 red is at ≈ (0.640, 0.330). Anything
    // meaningfully redder than BT.709 is treated as "wide gamut".
    if info.display_primaries[0].0 > 680_000 {
        info.best_named_primaries = ColorPrimaries::Bt2020;
    }

    // ---- CTA-861 extension block -----------------------------------
    //
    // Base block byte 126 = extension count. Extensions are 128 bytes
    // each and start at offset 128, 256, …
    let ext_count = base[126] as usize;
    for ext in edid
        .get(128..)
        .unwrap_or_default()
        .chunks_exact(128)
        .take(ext_count)
    {
        let Ok(ext): Result<&[u8; 128], _> = ext.try_into() else {
            break;
        };
        // CTA-861 extension: tag byte 0 == 0x02. First non-tag byte
        // is the revision, next is the DTD offset.
        if ext[0] != 0x02 {
            continue;
        }
        parse_cta861_data_blocks(ext, &mut info);
    }

    info
}

fn parse_cta861_data_blocks(ext: &[u8; 128], info: &mut HdrDisplayInfo) {
    // Data block collection starts at byte 4 and runs until the DTD
    // offset in byte 2. DTD offset == 0 → no DTDs, block runs to
    // the end minus the 1-byte checksum trailer.
    let dtd_offset = ext[2] as usize;
    let end = if dtd_offset == 0 {
        127 // exclude checksum
    } else {
        dtd_offset.min(127)
    };
    // A header byte carries its own payload length, so a malformed block can
    // claim more bytes than the extension holds. Every step is a checked `get`
    // and the cursor only moves forward, so a bad length ends the walk rather
    // than reading past the block.
    let mut i = 4usize;
    while i < end {
        let Some(&header) = ext.get(i) else { break };
        let tag = header >> 5;
        let len = (header & 0x1f) as usize;
        let Some(payload_start) = i.checked_add(1) else {
            break;
        };
        let Some(payload_end) = payload_start.checked_add(len) else {
            break;
        };
        if payload_end > end {
            break;
        }
        let Some(payload) = ext.get(payload_start..payload_end) else {
            break;
        };
        // Tag 7 = "Use Extended Tag". Byte 0 of the payload is the extended
        // tag; the rest is block-specific data.
        if tag == 7
            && let Some((&extended_tag, body)) = payload.split_first()
        {
            match extended_tag {
                5 => parse_colorimetry_data_block(body, info),
                6 => parse_hdr_static_metadata_block(body, info),
                _ => {}
            }
        }
        i = payload_end;
    }
}

fn parse_colorimetry_data_block(body: &[u8], info: &mut HdrDisplayInfo) {
    // CTA-861-G §7.5.5. Byte 0 bits: 0=xvYCC601, 1=xvYCC709,
    // 2=sYCC601, 3=opYCC601, 4=opRGB, 5=BT2020cYCC, 6=BT2020YCC,
    // 7=BT2020RGB. Any BT2020 bit set → wide-gamut display; we
    // upgrade the named primary hint.
    let Some(&first) = body.first() else {
        return;
    };
    let bt2020_mask = 0b1110_0000;
    if first & bt2020_mask != 0 {
        info.best_named_primaries = ColorPrimaries::Bt2020;
    }
}

fn parse_hdr_static_metadata_block(body: &[u8], info: &mut HdrDisplayInfo) {
    // CTA-861-G §7.5.13.
    // - Byte 0: ET0..=ET5 (EOTF bits): 0=SDR, 1=Traditional HDR,
    //   2=SMPTE ST 2084, 3=HLG.
    // - Byte 1: Static Metadata Descriptor support (bit 0 = SMD 1).
    // - Byte 2: max_luminance_code (Desired Content Max Luminance).
    // - Byte 3: max_frame_average_luminance_code.
    // - Byte 4: min_luminance_code.
    //
    // Luminance encoding per CTA-861-G:
    //   max     = 50 * 2^(code/32)                 cd/m²
    //   fall    = 50 * 2^(code/32)                 cd/m²
    //   min     = max_lum * (code/255)^2 / 100     cd/m²
    if body.is_empty() {
        return;
    }
    let Some(&eotf_bits) = body.first() else {
        return;
    };
    info.supported_transfers.clear();
    // Bit 0 — traditional gamma SDR (BT.1886 / gamma22 territory).
    if eotf_bits & 0b0000_0001 != 0 {
        info.supported_transfers
            .push(TransferCharacteristics::Bt709);
    }
    // Bit 1 — traditional HDR (rarely used; group with SDR gamma).
    if eotf_bits & 0b0000_0010 != 0
        && !info
            .supported_transfers
            .contains(&TransferCharacteristics::Bt709)
    {
        info.supported_transfers
            .push(TransferCharacteristics::Bt709);
    }
    // Bit 2 — SMPTE ST 2084 (PQ).
    if eotf_bits & 0b0000_0100 != 0 {
        info.supported_transfers
            .push(TransferCharacteristics::SmpteSt2084);
    }
    // Bit 3 — HLG.
    if eotf_bits & 0b0000_1000 != 0 {
        info.supported_transfers
            .push(TransferCharacteristics::AribStdB67);
    }

    // Luminance descriptors. Absent bytes = 0 = unknown; we leave
    // the defaults from `default_sdr` in place if the encoding says
    // "not populated".
    if let Some(&raw) = body.get(2).filter(|&&b| b != 0) {
        let code = f32::from(raw);
        let max_cd_m2 = 50.0 * 2f32.powf(code / 32.0);
        info.max_luminance_cd_m2 = max_cd_m2.round().min(10_000.0) as u16;
    }
    if let Some(&raw) = body.get(3).filter(|&&b| b != 0) {
        let code = f32::from(raw);
        let fall_cd_m2 = 50.0 * 2f32.powf(code / 32.0);
        info.max_frame_average_luminance_cd_m2 = fall_cd_m2.round().min(10_000.0) as u16;
    }
    if let Some(&raw) = body.get(4).filter(|&&b| b != 0) {
        // min is derived from max: min = (max_lum * (code/255)^2) / 100
        let max_lum = f32::from(info.max_luminance_cd_m2.max(1));
        let ratio = f32::from(raw) / 255.0;
        let min_cd_m2 = max_lum * ratio * ratio / 100.0;
        // Convert cd/m² → 1/10000 units (matching Hdr10 metadata).
        info.min_luminance_micro_cd_m2 = (min_cd_m2 * 10_000.0).round() as u32;
    }

    // Whenever a display advertises PQ or HLG, it must be a
    // wide-gamut panel by CTA-861 rules. Upgrade the named primary
    // hint if it hasn't already been.
    if info.supported_transfers.iter().any(|t| t.is_hdr()) {
        info.best_named_primaries = ColorPrimaries::Bt2020;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned HDR display EDID inspired by a Dell U2720QM / LG C1 — a
    /// 4K panel with a CTA-861 extension block advertising PQ + HLG,
    /// 1000 cd/m² peak, 400 cd/m² frame-average, 0.05 cd/m² min.
    ///
    /// Constructed by hand rather than copied from a real device
    /// dump so the fields we test on are deterministic and legally
    /// clear. Layout follows VESA E-EDID 1.4 + CTA-861-G exactly.
    #[allow(clippy::similar_names, reason = "R/G/B/W xy component names come from the EDID 1.4 chromaticity layout")]
    fn hdr_edid_blob() -> [u8; 256] {
        // Split a 10-bit chromaticity value into its (low 2, high 8) bit halves.
        fn split(v: u16) -> (u8, u8) {
            let hi = ((v >> 2) & 0xFF) as u8;
            let lo = (v & 0x3) as u8;
            (lo, hi)
        }

        let mut edid = [0u8; 256];
        // Base block --------------------------------------------------
        // 0..8 — EDID header.
        edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        // 8..10 — Manufacturer ID ("DEL"). Not verified by our parser.
        edid[8] = 0x10;
        edid[9] = 0xAC;
        // 18 — EDID version.
        edid[18] = 1;
        edid[19] = 4;
        // 24 — feature bitmap; irrelevant to us.
        edid[24] = 0x00;
        //
        // Chromaticities (bytes 25..=34):
        //
        // We want BT.2020 red (0.708, 0.292), green (0.170, 0.797),
        // blue (0.131, 0.046) and D65 white (0.3127, 0.329).
        //
        // 10-bit fixed-point → round(x * 1024):
        // - R: (725, 299)  = 0.7080, 0.2920
        // - G: (174, 816)  = 0.1699, 0.7969
        // - B: (134, 47)   = 0.1309, 0.0459
        // - W: (320, 337)  = 0.3125, 0.3291
        //
        // Then split into (lo_2, hi_8) pairs.
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
        // 126 — extension count.
        edid[126] = 1;
        // 127 — checksum (uncomputed; parser doesn't verify).
        //
        // Extension block (offset 128..256) ---------------------------
        edid[128] = 0x02; // ext[0] — CTA-861 tag
        edid[129] = 3; // ext[1] — CTA-861 revision 3
        //
        // ext[2] is the DTD offset and ext[3] is the flags byte (native
        // format count / IT-underscan / basic-audio / YCbCr support).
        // The data-block collection therefore starts at ext[4], NOT
        // ext[3] — omitting the flags byte shifts every block one byte
        // early and the parser reads the extended-tag payloads as block
        // headers, silently yielding an SDR result for an HDR panel.
        //
        // Block 1: Colorimetry Data Block (extended tag 5).
        // Header byte = (tag 7 << 5) | len. len = 3 (extended-tag byte
        // + 2 colorimetry bytes) → header = 0xE3. Occupies ext[4..8].
        //
        // Block 2: HDR Static Metadata Data Block (extended tag 6).
        // Payload = 1 (ext tag) + 5 (metadata) = 6 → header = 0xE6.
        // Occupies ext[8..15].
        //
        // DTD offset must point just past both = 15.
        edid[130] = 15;
        edid[131] = 0x00; // ext[3] — flags; zero for this synthetic panel
        //
        // Colorimetry block: ext[4..8] = edid[132..136].
        edid[132] = 0xE3;
        edid[133] = 0x05; // extended tag 5
        edid[134] = 0b1110_0000; // BT2020RGB + BT2020YCC + BT2020cYCC
        edid[135] = 0x00;
        //
        // HDR Static Metadata: ext[8..15] = edid[136..143].
        edid[136] = 0xE6;
        edid[137] = 0x06; // extended tag 6
        // EOTF: SDR (bit 0) + PQ (bit 2) + HLG (bit 3).
        edid[138] = 0b0000_1101;
        edid[139] = 0x01; // Static Metadata Type 1 supported.
        // max_luminance_code such that 50 * 2^(code/32) ≈ 1000.
        // code = 32 * log2(1000/50) ≈ 32 * 4.322 ≈ 138.
        edid[140] = 138;
        // frame-average ≈ 400 → code = 32 * log2(400/50) ≈ 32 * 3 = 96.
        edid[141] = 96;
        // min_luminance: 0.05 cd/m² given max=1000 →
        // min = max * (code/255)^2 / 100 → 0.05 = 1000 * (code/255)^2 / 100
        // → (code/255)^2 = 0.005 → code/255 ≈ 0.0707 → code ≈ 18.
        edid[142] = 18;

        edid
    }

    #[test]
    fn parses_bt2020_primaries() {
        let edid = hdr_edid_blob();
        let info = parse_hdr_display_info(&edid);
        // Red primary ~0.708, so x_micro should be around 708_000
        // (±3000 tolerance for the 10-bit fixed-point round-trip).
        let (rx, ry) = info.display_primaries[0];
        assert!(
            (i64::from(rx) - 708_000).abs() < 3_000,
            "red x should ≈ 708_000, got {rx}"
        );
        assert!(
            (i64::from(ry) - 292_000).abs() < 3_000,
            "red y should ≈ 292_000, got {ry}"
        );
        assert_eq!(info.best_named_primaries, ColorPrimaries::Bt2020);
    }

    #[test]
    fn parses_peak_and_min_luminance() {
        let edid = hdr_edid_blob();
        let info = parse_hdr_display_info(&edid);
        // Peak ≈ 1000 cd/m² (±30 for the rounding path).
        assert!(
            (i32::from(info.max_luminance_cd_m2) - 1000).abs() < 30,
            "expected ~1000 nit peak, got {}",
            info.max_luminance_cd_m2
        );
        // Frame-average ≈ 400 cd/m².
        assert!(
            (i32::from(info.max_frame_average_luminance_cd_m2) - 400).abs() < 20,
            "expected ~400 nit FALL, got {}",
            info.max_frame_average_luminance_cd_m2
        );
        // Min luminance in 1/10000 cd/m² units: 0.05 → 500.
        assert!(
            (info.min_luminance_micro_cd_m2 as i32 - 500).abs() < 200,
            "expected ~500 (0.05 cd/m²), got {}",
            info.min_luminance_micro_cd_m2
        );
    }

    #[test]
    fn advertises_pq_and_hlg_transfers() {
        let edid = hdr_edid_blob();
        let info = parse_hdr_display_info(&edid);
        assert!(
            info.supported_transfers
                .contains(&TransferCharacteristics::SmpteSt2084)
        );
        assert!(
            info.supported_transfers
                .contains(&TransferCharacteristics::AribStdB67)
        );
    }

    #[test]
    fn short_blob_returns_sdr_default() {
        // Empty and half-block inputs must not panic — SDR fallback.
        let info = parse_hdr_display_info(&[]);
        assert_eq!(info.best_named_primaries, ColorPrimaries::Bt709);
        assert_eq!(info.max_luminance_cd_m2, 100);

        let info = parse_hdr_display_info(&[0u8; 64]);
        assert_eq!(info.best_named_primaries, ColorPrimaries::Bt709);
    }

    #[test]
    fn no_extension_block_returns_base_chromaticities_only() {
        let mut edid = hdr_edid_blob();
        // Wipe the extension count so the parser never enters
        // the CTA-861 block.
        edid[126] = 0;
        let info = parse_hdr_display_info(&edid);
        // Base chromaticities parsed → the parser still classifies
        // this as BT.2020, but transfer set stays at the SDR default
        // (only Bt709).
        assert_eq!(info.best_named_primaries, ColorPrimaries::Bt2020);
        assert_eq!(
            info.supported_transfers,
            vec![TransferCharacteristics::Bt709]
        );
    }

    /// A display supplies its own EDID, so a malformed one must not take the
    /// compositor down. Before the parser used checked access, a data-block
    /// header claiming a payload longer than its block indexed straight past
    /// the end.
    #[test]
    fn a_malformed_blob_never_panics() {
        let good = hdr_edid_blob();
        for n in 0..good.len() {
            let _ = parse_hdr_display_info(&good[..n]);
        }

        for len_bits in 0..32u8 {
            let mut edid = hdr_edid_blob();
            edid[126] = 1;
            edid[128 + 2] = 0; // DTD offset 0: collection runs to the checksum
            edid[128 + 4] = (7 << 5) | len_bits; // extended tag, over-long payload
            let _ = parse_hdr_display_info(&edid);
        }

        let mut edid = hdr_edid_blob();
        edid[126] = 255; // more extensions than the blob carries
        let _ = parse_hdr_display_info(&edid);

        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for len in [0usize, 1, 127, 128, 129, 255, 256, 383] {
            let blob: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 24) as u8
                })
                .collect();
            let _ = parse_hdr_display_info(&blob);
        }
    }
}
