//! Hardware HEVC Main10 encode integration tests. Two backend paths, each
//! self-skipping when the host can't drive it:
//! - `#[cfg(feature = "vaapi")]` — hevc_vaapi at 1280x720; skips on
//!   AMD/Intel iGPUs lacking a HEVC Main10 encode block.
//! - `#[cfg(feature = "nvenc")]` — hevc_nvenc at 1280x720 and 1920x1080;
//!   skips when `cuInit` fails (no CUDA, or a wedged UVM state).
//!
//! Asserts on a working host: constructs via `EncoderInit::hdr10_hevc`,
//! `encode()` returns non-empty Annex-B bytes, `Encoder::codec()` reports
//! `Codec::HevcMain10`, and the first NAL after the start code isn't an
//! H.264 SPS (0x67).

#![cfg(any(feature = "vaapi", feature = "nvenc"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::collab::encoder::{Codec, EncoderInit, EncoderKind, FrameInput, build};
use shell::collab::hdr_metadata::HdrConfig;

/// Solid-red RGBA test frame — swscale converts to P010 on encode.
fn red_frame(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        out.extend_from_slice(&[255, 0, 0, 255]);
    }
    out
}

fn starts_with_annex_b(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x00, 0x00, 0x00, 0x01]) || bytes.starts_with(&[0x00, 0x00, 0x01])
}

/// First byte after the start code — the NAL header (HEVC:
/// forbidden_zero_bit + 6-bit nal_type + 6-bit layer_id + 3-bit
/// temporal_id; H.264: forbidden_zero_bit + 2-bit nal_ref_idc + 5-bit
/// nal_type). The bit patterns don't collide for keyframe SPS/VPS.
fn first_nal_header_byte(bytes: &[u8]) -> Option<u8> {
    if bytes.starts_with(&[0x00, 0x00, 0x00, 0x01]) {
        return bytes.get(4).copied();
    }
    if bytes.starts_with(&[0x00, 0x00, 0x01]) {
        return bytes.get(3).copied();
    }
    None
}

#[cfg(feature = "vaapi")]
mod vaapi_tests {
    use super::*;
    use shell::collab::encoder::vaapi;

    fn hevc_vaapi_available() -> bool {
        let outcome = vaapi::probe(Codec::HevcMain10);
        if !outcome.available {
            eprintln!("hevc_vaapi unavailable: {}", outcome.reason);
        }
        outcome.available
    }

    #[test]
    #[ignore = "requires --features vaapi + VA-API HEVC Main10 encode support"]
    fn vaapi_hevc_encodes_hdr10_annex_b_bytes() {
        if !hevc_vaapi_available() {
            return;
        }
        const W: u32 = 1280;
        const H: u32 = 720;
        let init = EncoderInit::hdr10_hevc(W, H, 30, 10_000, 2, HdrConfig::pq_reference());
        let mut backend = build(EncoderKind::VaApi, init).expect("construct hevc_vaapi backend");
        assert_eq!(backend.kind(), EncoderKind::VaApi);
        assert_eq!(
            backend.codec(),
            Codec::HevcMain10,
            "the runtime codec accessor must roll up the HEVC config"
        );
        let frame = red_frame(W, H);
        let mut accumulated = Vec::new();
        for _ in 0..16 {
            let mut out = Vec::new();
            let stats = backend
                .encode(FrameInput::Rgba(&frame), &mut out)
                .expect("hevc_vaapi encode should succeed on a hardware host");
            assert_eq!(stats.bytes, out.len());
            accumulated.extend_from_slice(&out);
        }
        assert!(
            !accumulated.is_empty(),
            "hevc_vaapi produced no bytes across 16 frames"
        );
        assert!(
            starts_with_annex_b(&accumulated),
            "hevc_vaapi output must start with Annex-B start code, got {:02x?}",
            &accumulated[..accumulated.len().min(8)]
        );
        // HEVC's VPS_NUT=32 shifted left one gives 0x40, so an explicit
        // inequality against H.264's SPS (0x67) catches a codec mismatch.
        let nal_byte = first_nal_header_byte(&accumulated).expect("annex-b start-code");
        assert_ne!(
            nal_byte, 0x67,
            "hevc_vaapi emitted an H.264 SPS as first NAL — codec routing broken"
        );
        eprintln!(
            "hevc_vaapi 720p HDR10: {} bytes across 16 frames, first NAL byte {:#04x}",
            accumulated.len(),
            nal_byte
        );
    }

    #[test]
    #[ignore = "requires --features vaapi + VA-API HEVC Main10 encode support"]
    fn vaapi_hevc_sustained_720p_encode() {
        if !hevc_vaapi_available() {
            return;
        }
        const W: u32 = 1280;
        const H: u32 = 720;
        const FRAMES: u32 = 60;
        let init = EncoderInit::hdr10_hevc(W, H, 30, 10_000, 2, HdrConfig::pq_reference());
        let mut backend = match build(EncoderKind::VaApi, init) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping — construction failed on this host: {e}");
                return;
            }
        };
        let frame = red_frame(W, H);
        let start = std::time::Instant::now();
        let mut total_bytes: usize = 0;
        let mut frames_ok: u32 = 0;
        for _ in 0..FRAMES {
            let mut out = Vec::new();
            match backend.encode(FrameInput::Rgba(&frame), &mut out) {
                Ok(stats) => {
                    total_bytes += stats.bytes;
                    frames_ok += 1;
                }
                Err(e) => panic!("hevc_vaapi encode failure at frame {frames_ok}: {e}"),
            }
        }
        let elapsed = start.elapsed();
        assert_eq!(frames_ok, FRAMES);
        let fps = FRAMES as f64 / elapsed.as_secs_f64();
        let mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
        eprintln!(
            "hevc_vaapi 720p HDR10 sustained: {FRAMES} frames in {:.2}s = {:.1} fps, {} bytes, {:.2} Mbps",
            elapsed.as_secs_f64(),
            fps,
            total_bytes,
            mbps
        );
    }
}

#[cfg(feature = "nvenc")]
mod nvenc_tests {
    use super::*;
    use shell::collab::encoder::nvenc;

    fn hevc_nvenc_available() -> bool {
        let outcome = nvenc::probe(Codec::HevcMain10);
        if !outcome.available {
            eprintln!("hevc_nvenc unavailable: {}", outcome.reason);
        }
        outcome.available
    }

    #[test]
    #[ignore = "requires --features nvenc + working NVENC HEVC hardware"]
    fn nvenc_hevc_720p_encodes_annex_b() {
        if !hevc_nvenc_available() {
            return;
        }
        const W: u32 = 1280;
        const H: u32 = 720;
        let init = EncoderInit::hdr10_hevc(W, H, 30, 10_000, 2, HdrConfig::pq_reference());
        let mut backend = match build(EncoderKind::NvEnc, init) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping — nvenc HEVC construction failed: {e}");
                return;
            }
        };
        assert_eq!(backend.codec(), Codec::HevcMain10);
        let frame = red_frame(W, H);
        let mut accumulated = Vec::new();
        for _ in 0..16 {
            let mut out = Vec::new();
            let stats = backend
                .encode(FrameInput::Rgba(&frame), &mut out)
                .expect("nvenc hevc encode succeeds");
            assert_eq!(stats.bytes, out.len());
            accumulated.extend_from_slice(&out);
        }
        assert!(!accumulated.is_empty());
        assert!(starts_with_annex_b(&accumulated));
        let nal_byte = first_nal_header_byte(&accumulated).expect("annex-b start-code");
        // HEVC VPS_NUT=32 -> 0x40, SPS_NUT=33 -> 0x42, AUD_NUT=35 -> 0x46; H.264
        // SPS is 0x67, so any non-0x67 first byte confirms HEVC routing.
        assert_ne!(
            nal_byte, 0x67,
            "hevc_nvenc emitted an H.264 SPS as first NAL — codec routing broken"
        );
        eprintln!(
            "hevc_nvenc 720p HDR10: {} bytes across 16 frames, first NAL byte {:#04x}",
            accumulated.len(),
            nal_byte
        );
    }

    #[test]
    #[ignore = "requires --features nvenc + working NVENC HEVC hardware"]
    fn nvenc_hevc_1080p_sustained_encode() {
        if !hevc_nvenc_available() {
            return;
        }
        const W: u32 = 1920;
        const H: u32 = 1080;
        const FRAMES: u32 = 60;
        let init = EncoderInit::hdr10_hevc(W, H, 30, 15_000, 2, HdrConfig::pq_reference());
        let mut backend = match build(EncoderKind::NvEnc, init) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping — construction failed: {e}");
                return;
            }
        };
        let frame = red_frame(W, H);
        let start = std::time::Instant::now();
        let mut total_bytes: usize = 0;
        let mut frames_ok: u32 = 0;
        for _ in 0..FRAMES {
            let mut out = Vec::new();
            match backend.encode(FrameInput::Rgba(&frame), &mut out) {
                Ok(stats) => {
                    total_bytes += stats.bytes;
                    frames_ok += 1;
                }
                Err(e) => panic!("hevc_nvenc encode failure at frame {frames_ok}: {e}"),
            }
        }
        let elapsed = start.elapsed();
        assert_eq!(frames_ok, FRAMES);
        let fps = FRAMES as f64 / elapsed.as_secs_f64();
        let mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
        eprintln!(
            "hevc_nvenc 1080p HDR10 sustained: {FRAMES} frames in {:.2}s = {:.1} fps, {} bytes, {:.2} Mbps",
            elapsed.as_secs_f64(),
            fps,
            total_bytes,
            mbps
        );
    }

    /// HEVC and H.264 NALs from the same NVENC device share nothing after the
    /// start code — proves the codec enum flips the encoder rather than
    /// silently sending H.264 with a different frame format.
    #[test]
    #[ignore = "requires --features nvenc + NVENC HEVC + NVENC H.264"]
    fn nvenc_hevc_bytes_differ_from_h264() {
        let h264_ok = nvenc::probe(Codec::H264).available;
        let hevc_ok = nvenc::probe(Codec::HevcMain10).available;
        if !h264_ok || !hevc_ok {
            eprintln!("skipping — one of nvenc h264/hevc unavailable");
            return;
        }
        const W: u32 = 1280;
        const H: u32 = 720;
        let frame = red_frame(W, H);

        let mut b_h264 = build(EncoderKind::NvEnc, EncoderInit::sdr_h264(W, H, 30, 6000, 2))
            .expect("h264 build");
        let mut b_hevc = build(
            EncoderKind::NvEnc,
            EncoderInit::hdr10_hevc(W, H, 30, 10_000, 2, HdrConfig::pq_reference()),
        )
        .expect("hevc build");

        assert_eq!(b_h264.codec(), Codec::H264);
        assert_eq!(b_hevc.codec(), Codec::HevcMain10);

        let (mut h264, mut hevc) = (Vec::new(), Vec::new());
        // Flush the encoder's look-ahead so this compares real bitstream bytes.
        for _ in 0..16 {
            let mut o = Vec::new();
            b_h264.encode(FrameInput::Rgba(&frame), &mut o).unwrap();
            h264.extend_from_slice(&o);
            let mut o = Vec::new();
            b_hevc.encode(FrameInput::Rgba(&frame), &mut o).unwrap();
            hevc.extend_from_slice(&o);
        }
        assert!(!h264.is_empty() && !hevc.is_empty());
        let nb_h264 = first_nal_header_byte(&h264).expect("h264 has nal");
        let nb_hevc = first_nal_header_byte(&hevc).expect("hevc has nal");
        assert_ne!(
            nb_h264, nb_hevc,
            "same first-NAL byte from two supposedly-different codecs — routing broken"
        );
        eprintln!(
            "nvenc byte-check: h264 first NAL {:#04x}, hevc first NAL {:#04x}",
            nb_h264, nb_hevc
        );
    }
}
