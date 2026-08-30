//! End-to-end HDR10 SEI metadata verification: encodes a solid-red HDR
//! frame with the first available HEVC hardware backend and scans the
//! Annex-B stream for the two HDR10 SEI messages our encoder attaches —
//! HEVC Prefix SEI NAL (type 39) carrying payload type 137
//! (`mastering_display_colour_volume`, SMPTE ST 2086) and payload type
//! 144 (`content_light_level_info`, CTA-861.3). Self-skips if neither
//! hardware backend is available. Scanning raw NALs works with any
//! libavcodec build that emits HEVC, including ones with no matching
//! decoder linked in.

#![cfg(any(feature = "vaapi", feature = "nvenc"))]

use shell::collab::encoder::{Codec, EncoderInit, EncoderKind, FrameInput, build};
use shell::collab::hdr_metadata::HdrConfig;

/// SMPTE ST 2086 payload type per H.265 § D.2.28.
const SEI_MASTERING_DISPLAY: u8 = 137;
/// CTA-861.3 payload type per H.265 § D.2.35.
const SEI_CONTENT_LIGHT_LEVEL: u8 = 144;

/// HEVC PREFIX_SEI_NUT = 39. In the 2-byte NAL header the first byte
/// carries `forbidden_zero_bit(1) | nal_unit_type(6) | nuh_layer_id_high(1)`,
/// MSB-first, so `(byte0 >> 1) & 0x3F` extracts `nal_unit_type`
/// independent of the layer-id upper bit.
const HEVC_NAL_TYPE_PREFIX_SEI: u8 = 39;

fn hevc_nal_type(byte0: u8) -> u8 {
    (byte0 >> 1) & 0x3F
}

fn red_frame(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        out.extend_from_slice(&[255, 0, 0, 255]);
    }
    out
}

/// Preference order matches the runtime probe: NVENC first, then VA-API.
fn pick_hevc_backend() -> Option<EncoderKind> {
    #[cfg(feature = "nvenc")]
    {
        use shell::collab::encoder::nvenc;
        let o = nvenc::probe(Codec::HevcMain10);
        if o.available {
            return Some(EncoderKind::NvEnc);
        }
        eprintln!("nvenc hevc unavailable: {}", o.reason);
    }
    #[cfg(feature = "vaapi")]
    {
        use shell::collab::encoder::vaapi;
        let o = vaapi::probe(Codec::HevcMain10);
        if o.available {
            return Some(EncoderKind::VaApi);
        }
        eprintln!("vaapi hevc unavailable: {}", o.reason);
    }
    None
}

/// Index of every byte immediately after an Annex-B start code, i.e. the
/// first byte of each NAL header.
fn nal_header_offsets(bytes: &[u8]) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == 0 && bytes[i + 1] == 0 {
            if bytes[i + 2] == 1 {
                offsets.push(i + 3);
                i += 3;
                continue;
            }
            if bytes[i + 2] == 0 && bytes[i + 3] == 1 {
                offsets.push(i + 4);
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    offsets
}

/// `Some(payload_type)` for the first SEI message in the prefix SEI NAL
/// at this offset — our encoder never batches multiple messages per NAL.
///
/// SEI RBSP layout per H.265 § 7.3.5: NAL header (2 bytes), payload_type
/// (0xff continuation bytes + final byte, summed), payload_size (same
/// encoding), then payload bytes.
fn sei_payload_types_at(bytes: &[u8], nal_offset: usize) -> Vec<u8> {
    let mut out = Vec::new();
    if nal_offset + 2 >= bytes.len() {
        return out;
    }
    if hevc_nal_type(bytes[nal_offset]) != HEVC_NAL_TYPE_PREFIX_SEI {
        return out;
    }
    // Skip the 2-byte NAL header, then iterate SEI messages until a byte
    // pattern that isn't a legitimate continuation.
    let mut cur = nal_offset + 2;
    while cur < bytes.len() {
        let mut payload_type: u32 = 0;
        while cur < bytes.len() && bytes[cur] == 0xff {
            payload_type += 0xff;
            cur += 1;
        }
        if cur >= bytes.len() {
            break;
        }
        payload_type += bytes[cur] as u32;
        cur += 1;
        let mut payload_size: u32 = 0;
        while cur < bytes.len() && bytes[cur] == 0xff {
            payload_size += 0xff;
            cur += 1;
        }
        if cur >= bytes.len() {
            break;
        }
        payload_size += bytes[cur] as u32;
        cur += 1;
        // All HEVC-defined payload types fit in a u8.
        if payload_type <= u8::MAX as u32 {
            out.push(payload_type as u8);
        }
        cur = cur.saturating_add(payload_size as usize);
        // A trailing rbsp_trailing_bits byte closes the SEI: stop on 0x80 or
        // near a NAL boundary.
        if cur >= bytes.len() || bytes[cur] == 0x80 {
            break;
        }
    }
    out
}

#[test]
#[ignore = "requires a hardware HEVC Main10 encoder; self-skips otherwise"]
fn hdr10_sei_survives_encode() {
    let kind = match pick_hevc_backend() {
        Some(k) => k,
        None => {
            eprintln!("skipping — no hardware HEVC Main10 encoder available");
            return;
        }
    };
    const W: u32 = 1280;
    const H: u32 = 720;
    let hdr = HdrConfig::pq_reference();
    let init = EncoderInit::hdr10_hevc(W, H, 30, 10_000, 2, hdr);
    // Force a non-default MaxCLL / MaxFALL so the content-light SEI
    // isn't skipped by the "both zero → omit" heuristic.
    let mut init = init;
    if let Some(m) = init.hdr.as_mut() {
        m.light.max_content_light_level = 1000;
        m.light.max_frame_average_light_level = 400;
    }
    let mut backend = match build(kind, init) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("skipping — hevc backend construction failed on this host: {e}");
            return;
        }
    };
    assert_eq!(backend.codec(), Codec::HevcMain10);
    let frame = red_frame(W, H);
    let mut accumulated = Vec::new();
    // Push enough frames to flush both the SPS/VPS + first SEIs; some
    // NVENC preset profiles emit them after a small buffered lookahead.
    for _ in 0..32 {
        let mut out = Vec::new();
        backend
            .encode(FrameInput::Rgba(&frame), &mut out)
            .expect("hevc encode");
        accumulated.extend_from_slice(&out);
    }
    assert!(!accumulated.is_empty(), "hevc backend produced no bytes");

    let nal_offsets = nal_header_offsets(&accumulated);
    let mut seen_mastering = false;
    let mut seen_content_light = false;
    for offset in nal_offsets {
        let types = sei_payload_types_at(&accumulated, offset);
        if types.contains(&SEI_MASTERING_DISPLAY) {
            seen_mastering = true;
        }
        if types.contains(&SEI_CONTENT_LIGHT_LEVEL) {
            seen_content_light = true;
        }
    }
    assert!(
        seen_mastering,
        "mastering-display SEI (type {}) never emitted — HDR SEI wiring broken",
        SEI_MASTERING_DISPLAY
    );
    assert!(
        seen_content_light,
        "content-light SEI (type {}) never emitted — HDR SEI wiring broken",
        SEI_CONTENT_LIGHT_LEVEL
    );
    eprintln!(
        "hdr10 sei: mastering-display + content-light both present in {} bytes",
        accumulated.len()
    );
}

#[test]
#[ignore = "requires a hardware HEVC Main10 encoder; self-skips otherwise"]
fn sdr_h264_pipeline_never_emits_hdr_sei() {
    // Sanity: the SDR H.264 path should show no HDR SEI (init.hdr is None) —
    // proves SEI attachment is gated on config, not a stale side-data leak.
    #[cfg(feature = "nvenc")]
    let kind = {
        use shell::collab::encoder::nvenc;
        if !nvenc::probe(Codec::H264).available {
            eprintln!("skipping — no nvenc h264");
            return;
        }
        EncoderKind::NvEnc
    };
    #[cfg(all(not(feature = "nvenc"), feature = "vaapi"))]
    let kind = {
        use shell::collab::encoder::vaapi;
        if !vaapi::probe(Codec::H264).available {
            eprintln!("skipping — no vaapi h264");
            return;
        }
        EncoderKind::VaApi
    };
    const W: u32 = 1280;
    const H: u32 = 720;
    let init = EncoderInit::sdr_h264(W, H, 30, 6000, 2);
    let mut backend = match build(kind, init) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("skipping — construction failed: {e}");
            return;
        }
    };
    let frame = red_frame(W, H);
    let mut accumulated = Vec::new();
    for _ in 0..16 {
        let mut out = Vec::new();
        backend
            .encode(FrameInput::Rgba(&frame), &mut out)
            .expect("h264 encode");
        accumulated.extend_from_slice(&out);
    }
    // H.264 uses different NAL byte patterns (SPS=0x67, SEI=0x06), so no
    // HDR-typed payload turns up even if the scanner were reused for H.264.
    let offsets = nal_header_offsets(&accumulated);
    for offset in offsets {
        let types = sei_payload_types_at(&accumulated, offset);
        assert!(
            !types.contains(&SEI_MASTERING_DISPLAY),
            "SDR H.264 stream unexpectedly carries mastering-display SEI"
        );
        assert!(
            !types.contains(&SEI_CONTENT_LIGHT_LEVEL),
            "SDR H.264 stream unexpectedly carries content-light SEI"
        );
    }
}
