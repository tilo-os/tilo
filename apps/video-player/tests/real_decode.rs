//! Real `GStreamer` decode proof for the local video player.
//!
//! Compiled only with `--features decode` (which turns on `media/decode`, the
//! real `playbin`→`appsink` pipeline). It synthesizes a tiny real media file on
//! disk with `GStreamer`'s own encoders, decodes it back through the exact engine
//! the app uses, and asserts that genuine RGBA frames are produced and their
//! presentation timestamps advance — i.e. the file actually plays.
//!
//! These tests require a working `GStreamer` runtime, `gst-launch-1.0`, and an
//! encoder/muxer from the candidates below. Missing prerequisites fail the test;
//! synthesized frames or an unobserved decoder cannot establish real playback.

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
use ui::time::Duration;

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
         videoconvert ! {encoder_muxer} ! filesink"
    );
    // `-e` sends EOS on finish so muxers finalize their headers; `-q` quiets it.
    let mut cmd = std::process::Command::new(if cfg!(windows) {
        "gst-launch-1.0.exe"
    } else {
        "gst-launch-1.0"
    });
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: this is a headless fixture.
    }
    cmd.arg("-e").arg("-q");
    for tok in desc.split_whitespace() {
        cmd.arg(tok);
    }
    // Keep the filename one argument and quote it for GStreamer's own parser.
    // Otherwise whitespace splits the path and Windows backslashes are eaten
    // as parser escapes even though no command shell is involved.
    let location = sample
        .to_str()
        .expect("UTF-8 scratch path")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    cmd.arg(format!("location=\"{location}\""));
    let ok = match cmd.status() {
        Ok(status) => status.success(),
        Err(error) => {
            eprintln!("could not launch GStreamer fixture encoder: {error}");
            false
        }
    };
    ok && sample.metadata().is_ok_and(|m| m.len() > 0)
}

/// Produce a small real media file, trying formats in order of how ubiquitous
/// their `GStreamer` plugins are. `None` when the host has no usable encoder.
/// The file we make is decodable by the same host (having
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
    let nanos = ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "tilo-vp-real-decode 日本語 #%-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        nanos
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn real_file_decodes_and_reaches_the_player_surface() {
    // Case `engine_decodes_a_real_file_and_the_pts_advances`.
    {
        let dir = scratch_dir();
        let sample = write_sample_media(&dir)
            .expect("real decode requires gst-launch-1.0 and a working encoder/muxer");

        let mut engine = MediaEngine::with_config(media::EngineConfig::default().silent());
        assert!(engine.is_real_decode(), "real GStreamer decoder required");

        engine.open(Source::LocalFile(sample.clone()));
        engine.play();

        // Pump until several distinct frame PTS are observed (or we time out).
        let mut ptss: Vec<u64> = Vec::new();
        let mut saw_duration = false;
        let mut first_frame_dims: Option<(u32, u32)> = None;
        let deadline = tilo_test_env::hang_deadline();
        while ui::time::now() < deadline {
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

        drop(engine);
        std::fs::remove_dir_all(&dir).expect("release and remove decoded sample");

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
    // Case `player_app_pulls_a_real_decoded_frame_onto_its_surface`.
    {
        let dir = scratch_dir();
        let sample = write_sample_media(&dir)
            .expect("real decode requires gst-launch-1.0 and a working encoder/muxer");

        // Drive the real app entry path: open the file, then pump `sync` (the live
        // event loop's per-frame call) until a decoded picture lands on the surface.
        let mut app = PlayerApp::new().with_open_path(&sample);
        assert!(
            app.player()
                .engine()
                .is_some_and(MediaEngine::is_real_decode),
            "the app must use the real decoder"
        );
        let mut doc = automerge::AutoCommit::new();
        let root = automerge::ROOT;

        let deadline = tilo_test_env::hang_deadline();
        while ui::time::now() < deadline {
            app.sync(&mut doc, &root);
            if app.player().latest_frame().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

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
        drop(app);
        std::fs::remove_dir_all(&dir).expect("release and remove decoded sample");
    }
}
