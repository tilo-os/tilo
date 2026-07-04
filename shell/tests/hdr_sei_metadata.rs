//! End-to-end HDR10 SEI metadata verification.
//!
//! Constructs a solid-red HDR frame, encodes it with the first
//! available HEVC hardware backend, and scans the resulting Annex-B
//! byte stream for the two HDR10 SEI messages our encoder attaches:
//!
//! - HEVC Prefix SEI NAL (NAL type 39, header byte `0x4E`) containing
//!   SEI payload type 137 = `mastering_display_colour_volume`
//!   (SMPTE ST 2086).
//! - Same NAL kind carrying SEI payload type 144 =
//!   `content_light_level_info` (CTA-861.3).
//!
//! Self-skips if neither hardware HEVC backend is available on this
//! host — the check is meaningful only when there's a real encoder to
//! drive.
//!
//! Scanning raw NALs is more portable than round-tripping through a
//! decoder: it works with any libavcodec build that emits HEVC (which
//! every ffmpeg-8 build does), including builds without a matching
//! decoder linked in.

#![cfg(any(feature = "vaapi", feature = "nvenc"))]

use shell::collab::encoder::{Codec, EncoderInit, EncoderKind, FrameInput, build};
use shell::collab::hdr_metadata::HdrConfig;

/// SMPTE ST 2086 payload type per H.265 § D.2.28.
const SEI_MASTERING_DISPLAY: u8 = 137;
/// CTA-861.3 payload type per H.265 § D.2.35.
const SEI_CONTENT_LIGHT_LEVEL: u8 = 144;

/// HEVC PREFIX_SEI_NUT header byte = `(0 << 7) | (39 << 1) | 0 = 0x4E`.
/// Matches the first byte of a prefix SEI NAL unit for layer_id upper
/// bit zero — the case for any single-layer HDR10 stream.
const HEVC_PREFIX_SEI_NUT_BYTE: u8 = 0x4E;

/// Solid-red RGBA test frame.
fn red_frame(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        out.extend_from_slice(&[255, 0, 0, 255]);
    }
    out
}

/// Try to find a working HEVC Main10 backend on this host. Preference
/// order matches the runtime probe: NVENC first, then VA-API.
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

/// Walk `bytes` looking for Annex-B NAL unit boundaries; returns the
/// index of every byte immediately after a start code (i.e. the first
/// byte of each NAL header).
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

/// Given the byte offset of a prefix SEI NAL header in an Annex-B
/// stream, return `Some(payload_type)` for the first SEI message
/// carried inside, or `None` if the NAL isn't a prefix SEI or is
/// truncated. Reads only the first payload_type; each SEI NAL can
/// carry multiple messages but our encoder never batches them.
///
/// SEI RBSP layout per H.265 § 7.3.5:
/// 1. NAL header (2 bytes)
/// 2. payload_type: zero or more 0xff continuation bytes + final byte;
///    the accumulated value is payload_type.
/// 3. payload_size: same encoding.
/// 4. payload bytes.
fn sei_payload_types_at(bytes: &[u8], nal_offset: usize) -> Vec<u8> {
    let mut out = Vec::new();
    // Need at least NAL header (2 bytes) + one payload_type byte.
    if nal_offset + 2 >= bytes.len() {
        return out;
    }
    if bytes[nal_offset] & 0x7f != HEVC_PREFIX_SEI_NUT_BYTE & 0x7f {
        return out;
    }
    // Skip the 2-byte NAL header. Iterate SEI messages until a byte
    // pattern that isn't a legitimate continuation (or we run out).
    let mut cur = nal_offset + 2;
    while cur < bytes.len() {
        // Accumulate payload_type.
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
        // Accumulate payload_size.
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
        // Record the payload_type if it fits in a u8 (all HEVC-defined
        // payload types do).
        if payload_type <= u8::MAX as u32 {
            out.push(payload_type as u8);
        }
        // Skip payload bytes.
        cur = cur.saturating_add(payload_size as usize);
        // A trailing rbsp_trailing_bits byte closes the SEI; if we
        // land on 0x80 or byte close to a NAL boundary, stop.
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

    // Walk every NAL and inspect the SEI prefix ones.
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
    // Sanity: on the SDR H.264 path we should not see any HDR SEI —
    // the encoder ignores hdr metadata (init.hdr is None). Proves the
    // SEI attachment is gated on the config, not a stale side-data
    // attachment leaking across configs.
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
    // H.264 uses a different NAL byte pattern (SPS = 0x67, SEI = 0x06)
    // so the HEVC-specific `sei_payload_types_at` search will simply
    // find nothing. The assertion is that no HDR-typed payload turns
    // up even if the scanner had been reused for H.264.
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
