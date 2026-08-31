//! Real `GStreamer` decode proof for the local video player.
//!
//! Compiled only with `--features decode` (which turns on `media/decode`, the
//! real `playbin`→`appsink` pipeline). It synthesizes a tiny real media file on
//! disk with `GStreamer`'s own encoders, decodes it back through the exact engine
//! the app uses, and asserts that genuine RGBA frames are produced and their
//! presentation timestamps advance — i.e. the file actually plays.
//!
//! Honest gate, matching the engine's own `is_real_decode` fallback: if the host
//! has no usable encoder/muxer (so no sample can be made) or `GStreamer` can't
//! initialise at runtime, the test prints why and returns instead of asserting.
//! It never weakens a check and is never `#[ignore]`d — when a real decode stack
//! is present (the CI/host that runs `--features decode`) the assertions bite.

#![cfg(feature = "decode")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use media::{MediaEngine, Source};
use ui::widget::Widget; // brings `sync` (the per-frame Widget method) into scope
use video_player::PlayerApp;

/// Encode a ~1s 160x120 test clip to `sample` via `videotestsrc` + the given
/// encoder/muxer tail, using the `gst-launch-1.0` CLI. Returns whether a
/// non-empty file was produced.
fn try_encode(sample: &Path, encoder_muxer: &str) -> bool {
    let desc = format!(
        "videotestsrc num-buffers=25 ! \
         video/x-raw,width=160,height=120,framerate=25/1 ! \
         videoconvert ! {encoder_muxer} ! filesink location={}",
        sample.display()
    );
    // `-e` sends EOS on finish so muxers finalize their headers; `-q` quiets it.
    let mut cmd = std::process::Command::new("gst-launch-1.0");
    cmd.arg("-e").arg("-q");
    for tok in desc.split_whitespace() {
        cmd.arg(tok);
    }
    let ok = matches!(cmd.status(), Ok(s) if s.success());
    ok && sample.metadata().is_ok_and(|m| m.len() > 0)
}

/// Produce a small real media file, trying formats in order of how ubiquitous
/// their `GStreamer` plugins are. `None` when the host has no usable encoder —
/// the honest skip path. The file we make is decodable by the same host (having
/// an encoder for a format essentially guarantees the matching decoder), so a
/// produced sample is a fair test of the real decode path.
fn write_sample_media(dir: &Path) -> Option<PathBuf> {
    let candidates = [
        ("sample.ogv", "theoraenc ! oggmux"),
        ("sample.webm", "vp8enc ! webmmux"),
        ("sample.mkv", "vp8enc ! matroskamux"),
        ("sample.avi", "jpegenc ! avimux"),
        ("sample.mp4", "x264enc ! mp4mux"),
    ];
    for (name, tail) in candidates {
        let path = dir.join(name);
        if try_encode(&path, tail) {
            return Some(path);
        }
        let _ = std::fs::remove_file(&path);
    }
    None
}

/// A private scratch directory for this test run, cleaned up at the end.
fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "qos-vp-real-decode-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        nanos
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn engine_decodes_a_real_file_and_the_pts_advances() {
    let dir = scratch_dir();
    let Some(sample) = write_sample_media(&dir) else {
        eprintln!(
            "real_decode: no usable GStreamer encoder/muxer on this host — \
             cannot synthesize a sample; skipping real-decode assertions"
        );
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };

    let mut engine = MediaEngine::new();
    if !engine.is_real_decode() {
        eprintln!(
            "real_decode: GStreamer runtime unavailable — engine fell back to the \
             synthesized backend; skipping real-decode assertions"
        );
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    engine.open(Source::LocalFile(sample.clone()));
    engine.play();

    // Pump until several distinct frame PTS are observed (or we time out).
    let mut ptss: Vec<u64> = Vec::new();
    let mut saw_duration = false;
    let mut first_frame_dims: Option<(u32, u32)> = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        for ev in engine.poll() {
            if let media::MediaEvent::DurationKnown { .. } = ev {
                saw_duration = true;
            }
        }
        if let Some(frame) = engine.latest_frame() {
            // Every decoded frame is tightly-packed RGBA of a real size.
            assert!(
                frame.width > 0 && frame.height > 0,
                "decoded frame has real dimensions"
            );
            assert_eq!(
                frame.rgba.len(),
                frame.width as usize * frame.height as usize * 4,
                "tightly-packed RGBA8"
            );
            first_frame_dims.get_or_insert((frame.width, frame.height));
            if ptss.last() != Some(&frame.pts_ms) {
                ptss.push(frame.pts_ms);
            }
        }
        if ptss.len() >= 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        ptss.len() >= 2,
        "expected multiple decoded frames from {}, saw PTS {ptss:?}",
        sample.display()
    );
    // PTS is monotonic and actually advances across the decoded frames.
    for pair in ptss.windows(2) {
        assert!(pair[1] >= pair[0], "PTS must not go backwards: {ptss:?}");
    }
    assert!(
        ptss.iter().max() > ptss.iter().min(),
        "the playhead advanced across decoded frames: {ptss:?}"
    );
    assert!(
        saw_duration,
        "the stream's duration became known during decode"
    );
    let (w, h) = first_frame_dims.expect("at least one decoded frame");
    assert_eq!(
        (w, h),
        (160, 120),
        "decoded the frame at the encoded resolution"
    );
}

#[test]
fn player_app_pulls_a_real_decoded_frame_onto_its_surface() {
    let dir = scratch_dir();
    let Some(sample) = write_sample_media(&dir) else {
        eprintln!("real_decode: no encoder available; skipping the end-to-end app decode");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    if !MediaEngine::new().is_real_decode() {
        eprintln!("real_decode: GStreamer runtime unavailable; skipping the end-to-end app decode");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    // Drive the real app entry path: open the file, then pump `sync` (the live
    // event loop's per-frame call) until a decoded picture lands on the surface.
    let mut app = PlayerApp::new().with_open_path(&sample);
    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        app.sync(&mut doc, &root);
        if app.player().latest_frame().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        app.player().latest_frame().is_some(),
        "the app decoded {} and put a frame on the video surface",
        sample.display()
    );
    assert!(
        app.player().is_playing(),
        "the app autoplays the opened file"
    );
    assert!(
        app.player().duration_ms() > 0,
        "the app learned the stream duration"
    );
}
