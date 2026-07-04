//! Hardware NVENC encode integration tests.
//!
//! Requires the `nvenc` cargo feature. Self-skips if the host has no
//! working H.264 NVENC encoder — `/dev/nvidia0` might exist without
//! usable CUDA (containers stripped of CAP_SYS_ADMIN, etc.).
//!
//! Rootless podman drops the capability CUDA needs to init. To exercise
//! this on a distrobox-built binary, compile inside distrobox then run
//! the test executable directly on the host with the ffmpeg libs
//! pointed to via `LD_LIBRARY_PATH`.

#![cfg(feature = "nvenc")]

use shell::collab::encoder::{Codec, EncoderInit, EncoderKind, FrameInput, build, nvenc, probe};
use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::config::{EncoderChoice, FlatpakShareConfig};

fn nvenc_available() -> bool {
    let outcome = nvenc::probe(Codec::H264);
    if !outcome.available {
        eprintln!("nvenc unavailable: {}", outcome.reason);
    }
    outcome.available
}

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

#[test]
#[ignore = "requires --features nvenc + working NVENC hardware"]
fn nvenc_produces_annex_b_h264_bytes() {
    if !nvenc_available() {
        return;
    }
    // 1280x720 rather than a smaller frame because ffmpeg 8's
    // h264_nvenc wrapper rejects sub-720p CBR+baseline config on some
    // driver versions with a bare "Unknown error" from `avcodec_open2`.
    // The single-frame path we're asserting on doesn't care about the
    // pixel count.
    const W: u32 = 1280;
    const H: u32 = 720;
    let init = EncoderInit::sdr_h264(W, H, 30, 6000, 2);
    let mut backend = build(EncoderKind::NvEnc, init).expect("construct nvenc backend");
    assert_eq!(backend.kind(), EncoderKind::NvEnc);
    let frame = red_frame(W, H);
    let mut accumulated = Vec::new();
    let mut first_seen_at: Option<u32> = None;
    for i in 0..16 {
        let mut out = Vec::new();
        let stats = backend
            .encode(FrameInput::Rgba(&frame), &mut out)
            .expect("nvenc encode should succeed on a hardware host");
        assert_eq!(stats.bytes, out.len());
        if !out.is_empty() && first_seen_at.is_none() {
            first_seen_at = Some(i);
        }
        accumulated.extend_from_slice(&out);
    }
    assert!(
        !accumulated.is_empty(),
        "nvenc produced no bytes across 16 frames"
    );
    assert!(
        starts_with_annex_b(&accumulated),
        "output must start with H.264 Annex-B start code, got {:02x?}",
        &accumulated[..accumulated.len().min(8)]
    );
    eprintln!(
        "nvenc emitted first packet at frame #{} (buffered look-ahead), {} total bytes across 16 frames",
        first_seen_at.expect("some frame must produce bytes"),
        accumulated.len()
    );
}

#[test]
#[ignore = "requires --features nvenc + working NVENC hardware"]
fn nvenc_sustained_720p_encode() {
    if !nvenc_available() {
        return;
    }
    const W: u32 = 1280;
    const H: u32 = 720;
    const FRAMES: u32 = 120;
    let init = EncoderInit::sdr_h264(W, H, 30, 8000, 2);
    let mut backend = match build(EncoderKind::NvEnc, init) {
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
    let mut worst_wallclock = std::time::Duration::ZERO;
    for _ in 0..FRAMES {
        let mut out = Vec::new();
        match backend.encode(FrameInput::Rgba(&frame), &mut out) {
            Ok(stats) => {
                total_bytes += stats.bytes;
                frames_ok += 1;
                if stats.encode_wallclock > worst_wallclock {
                    worst_wallclock = stats.encode_wallclock;
                }
            }
            Err(e) => {
                panic!("nvenc encode failure at frame {frames_ok}: {e}");
            }
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(frames_ok, FRAMES);
    let fps = FRAMES as f64 / elapsed.as_secs_f64();
    let mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
    eprintln!(
        "nvenc 720p sustained: {FRAMES} frames in {:.2}s = {:.1} fps, {} bytes, {:.2} Mbps, worst frame {}us",
        elapsed.as_secs_f64(),
        fps,
        total_bytes,
        mbps,
        worst_wallclock.as_micros()
    );
}

#[test]
#[ignore = "requires --features nvenc + working NVENC hardware"]
fn nvenc_sustained_1080p_encode() {
    if !nvenc_available() {
        return;
    }
    const W: u32 = 1920;
    const H: u32 = 1080;
    const FRAMES: u32 = 60;
    let init = EncoderInit::sdr_h264(W, H, 30, 12000, 2);
    let mut backend = match build(EncoderKind::NvEnc, init) {
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
            Err(e) => {
                panic!("nvenc encode failure at frame {frames_ok}: {e}");
            }
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(frames_ok, FRAMES);
    let fps = FRAMES as f64 / elapsed.as_secs_f64();
    let mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
    eprintln!(
        "nvenc 1080p sustained: {FRAMES} frames in {:.2}s = {:.1} fps, {} bytes, {:.2} Mbps",
        elapsed.as_secs_f64(),
        fps,
        total_bytes,
        mbps
    );
}

#[test]
#[ignore = "requires --features nvenc + working NVENC hardware"]
fn shared_flatpak_session_rolls_up_nvenc() {
    if !nvenc_available() {
        return;
    }
    let cfg = FlatpakShareConfig {
        max_bitrate_kbps: 6000,
        target_fps: 30,
        encoder: EncoderChoice::Auto,
        hdr: None,
    };
    // 1280x720 for the same driver-config reason as
    // nvenc_produces_annex_b_h264_bytes.
    let session =
        SharedFlatpakSession::start(0xCCCC, "org.gimp.GIMP", 42, 1280, 720, cfg).expect("session");
    let (chosen_probe, _) = probe(Codec::H264, Some(EncoderChoice::Nvenc));
    assert_eq!(chosen_probe, EncoderKind::NvEnc);
    assert_eq!(session.chosen_encoder(), EncoderKind::NvEnc);
}
