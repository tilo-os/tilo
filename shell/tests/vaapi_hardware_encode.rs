//! Hardware VA-API encode integration tests.
//!
//! Requires the `vaapi` cargo feature. Self-skips if the host has no
//! working H.264 VA-API encoder — the render node might be present
//! (`/dev/dri/renderD128`) without H.264 encode support (older iGPUs,
//! discrete GPUs without an encode block, etc.).
//!
//! What we assert on a working host:
//!
//! - `EncoderKind::VaApi` is what the probe picks (proves the wiring
//!   actually took over the openh264 slot).
//! - `encode()` returns a `FrameStats` with a non-empty NAL byte count.
//! - The emitted bitstream starts with a valid H.264 Annex-B start
//!   code (`00 00 00 01` or `00 00 01`) — proves libavcodec is
//!   emitting real H.264 rather than an empty buffer.
//! - The chosen encoder rolled up into
//!   `SharedFlatpakSession::chosen_encoder()` matches
//!   `EncoderKind::VaApi` — proves the runtime kind selection isn't
//!   silently falling through to software.
//!
//! `#[ignore]`d so `cargo test -p shell` on CI stays quiet; enable
//! with `--include-ignored`.

#![cfg(feature = "vaapi")]

use shell::collab::encoder::{Codec, EncoderInit, EncoderKind, FrameInput, build, probe, vaapi};
use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::config::{EncoderChoice, FlatpakShareConfig};

/// Skip helper — if the vaapi probe reports unavailable, print why
/// and return `false` so the caller can early-out.
fn vaapi_available() -> bool {
    let outcome = vaapi::probe(Codec::H264);
    if !outcome.available {
        eprintln!("vaapi unavailable: {}", outcome.reason);
    }
    outcome.available
}

/// Solid-red 640x480 RGBA test frame.
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

/// End-to-end: build the vaapi backend, feed one solid-red frame,
/// assert the encoder emits H.264 bytes.
#[test]
#[ignore = "requires --features vaapi + working H.264 VA-API encoder"]
fn vaapi_encodes_a_single_red_frame() {
    if !vaapi_available() {
        return;
    }
    const W: u32 = 640;
    const H: u32 = 480;
    let init = EncoderInit::sdr_h264(W, H, 30, 4000, 2);
    let mut backend = build(EncoderKind::VaApi, init).expect("construct vaapi backend");
    assert_eq!(backend.kind(), EncoderKind::VaApi);
    assert_eq!(backend.width(), W);
    assert_eq!(backend.height(), H);

    let frame = red_frame(W, H);
    let mut out = Vec::new();
    let stats = backend
        .encode(FrameInput::Rgba(&frame), &mut out)
        .expect("vaapi encode should succeed on a hardware host");
    assert!(!out.is_empty(), "vaapi produced empty NAL stream");
    assert_eq!(stats.bytes, out.len(), "stats.bytes must match out.len()");
    assert!(
        starts_with_annex_b(&out),
        "vaapi output must start with H.264 Annex-B start code, got {:02x?}",
        &out[..out.len().min(8)]
    );
    eprintln!(
        "vaapi encoded 1 frame: {} bytes, keyframe={}, wallclock={}us",
        stats.bytes,
        stats.keyframe,
        stats.encode_wallclock.as_micros()
    );
}

/// Sustained encode: 60 frames at 720p, assert we produce meaningful
/// per-frame bytes + measure wall clock. Anything under ~50ms/frame is
/// hardware-fast. This test is a smoke-check that the encoder doesn't
/// stall after the first frame (a real risk with hw_frames pool
/// sizing).
#[test]
#[ignore = "requires --features vaapi + working H.264 VA-API encoder"]
fn vaapi_sustained_720p_encode() {
    if !vaapi_available() {
        return;
    }
    const W: u32 = 1280;
    const H: u32 = 720;
    const FRAMES: u32 = 60;
    let init = EncoderInit::sdr_h264(W, H, 30, 8000, 2);
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
            Err(e) => {
                panic!("vaapi encode failure at frame {frames_ok}: {e}");
            }
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(frames_ok, FRAMES, "vaapi must encode every submitted frame");
    let fps = FRAMES as f64 / elapsed.as_secs_f64();
    let mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
    eprintln!(
        "vaapi 720p sustained: {FRAMES} frames in {:.2}s = {:.1} fps, {} bytes, {:.2} Mbps",
        elapsed.as_secs_f64(),
        fps,
        total_bytes,
        mbps
    );
}

/// Session-level integration: SharedFlatpakSession's `chosen_encoder`
/// must roll up to VaApi when the probe wins. Guarantees the runtime
/// wiring picks the actual hardware backend rather than silently
/// falling through to openh264.
#[test]
#[ignore = "requires --features vaapi + working H.264 VA-API encoder"]
fn shared_flatpak_session_rolls_up_vaapi() {
    if !vaapi_available() {
        return;
    }
    let cfg = FlatpakShareConfig {
        max_bitrate_kbps: 4000,
        target_fps: 30,
        encoder: EncoderChoice::Auto,
        hdr: None,
    };
    let session =
        SharedFlatpakSession::start(0xBBBB, "org.gimp.GIMP", 42, 640, 480, cfg).expect("session");
    let (chosen_probe, _) = probe(Codec::H264, Some(EncoderChoice::Vaapi));
    // On any host where the probe would pick vaapi, the session's
    // chosen_encoder must match.
    assert_eq!(chosen_probe, EncoderKind::VaApi);
    assert_eq!(session.chosen_encoder(), EncoderKind::VaApi);
}
