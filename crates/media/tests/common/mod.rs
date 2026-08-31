//! Shared fixtures for the decode integration tests.
//!
//! # These tests do not skip
//!
//! A check that quietly passes when it could not observe the thing it claims
//! to check is worthless — it reads exactly like a real pass. So nothing here
//! returns "skipped": a missing clip, a `GStreamer` that will not initialise, or
//! a pipeline that produces no frame all **fail**, with a message saying what
//! was missing.
//!
//! Where the environment genuinely varies — a machine with no hardware decoder
//! for H.264 is a legitimate configuration — the test still asserts, but it
//! asserts the *correct outcome for the machine it is on*, having first
//! interrogated the registry. "There was no hardware decoder available" is a
//! conclusion the test proves and prints, never an assumption it makes to get
//! out of asserting.
//!
//! That covenant is why this module panics on purpose. `assert!` and `panic!`
//! here are the check, not a hazard — clippy's exemption for test code only
//! reaches `#[test]` functions, and every one of these is called from one.
#![allow(
    clippy::panic,
    reason = "a fixture helper that cannot observe its subject must fail loudly; \
              see the module docs above"
)]
#![allow(
    dead_code,
    reason = "each integration test binary compiles this whole module but uses \
              only the fixtures it needs — `hardware_decode` never measures a \
              seek drift, `software_fallback` never diffs two pictures. The \
              unused half is live in a sibling binary, not dead"
)]

// Everything below is reached only from the decode-gated test modules, so it
// carries the same gate they do: without it a hermetic build (no gstreamer dev
// stack, `--features decode` off) compiles a fixture loader nothing can call
// and warns about every item in the file.
#[cfg(feature = "decode")]
use std::path::PathBuf;

/// Frames per second of the fixture clip.
#[cfg(feature = "decode")]
pub const FIXTURE_FPS: f64 = 30.0;

/// One frame interval, milliseconds. A seek that lands within this of its
/// target landed on the right picture.
#[cfg(feature = "decode")]
pub const FRAME_MS: u64 = 33;

/// The fixture's keyframe interval. Verified with
/// `ffprobe -skip_frame nokey`: keyframes sit at 0, 8.333, 16.667, 25,
/// 33.333, 41.667 and 50 seconds and nowhere else.
///
/// This is what makes the accurate-seek test sharp. A `KEY_UNIT` seek to a
/// time in the middle of a GOP cannot do better than the nearest of those, so
/// the difference between snapping and decoding to the target is measured in
/// **seconds**, not milliseconds.
#[cfg(feature = "decode")]
pub const FIXTURE_GOP_MS: u64 = 8_333;

/// The fixture clip: 1080p H.264 in MP4, 30fps, 57.2s, 1717 frames.
#[cfg(feature = "decode")]
pub fn fixture_path() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.claude/demo-artifacts/qos-demo-57s.mp4");
    // Resolve the `..` segments: a `file://` URI keeps them verbatim and not
    // every source element normalises them.
    p.canonicalize().unwrap_or(p)
}

/// The fixture, failing loudly if it is not there. A decode test with no clip
/// has observed nothing and must not report success.
#[cfg(feature = "decode")]
pub fn require_fixture() -> PathBuf {
    let p = fixture_path();
    assert!(
        p.exists(),
        "decode fixture missing at {}. These tests verify real hardware decode \
         against a real H.264 clip; without it nothing is being checked, so this \
         is a failure rather than a skip.",
        p.display()
    );
    p
}

/// The fixture as a [`media::Source`].
#[cfg(feature = "decode")]
pub fn fixture_source() -> media::Source {
    media::Source::LocalFile(require_fixture())
}

/// A one-line description of every video decoder the registry offers, for a
/// failure message. Printing this is what turns "software was chosen" from an
/// unexplained result into a diagnosable one.
#[cfg(feature = "decode")]
pub fn registry_summary() -> String {
    let all = media::gst_backend::available_video_decoders();
    if all.is_empty() {
        return "registry has no video decoders at all".to_string();
    }
    let h264 = media::gst_backend::decoders_for("video/x-h264");
    let list = |v: &[media::hw::DecoderEntry]| -> String {
        v.iter()
            .map(|d| format!("{}({}, rank {})", d.name, d.kind.as_str(), d.rank))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{} video decoders total; H.264-capable: [{}]",
        all.len(),
        list(&h264)
    )
}

/// Whether this machine has any hardware decoder that can take H.264.
#[cfg(feature = "decode")]
pub fn has_hardware_h264() -> bool {
    media::gst_backend::decoders_for("video/x-h264")
        .iter()
        .any(|d| d.kind.is_hardware())
}

/// The presentation timestamp of the fixture's **first** decoded picture.
///
/// This clip's video track does not start at zero: its first buffer carries a
/// PTS of 66.67ms (two frames), an offset the container's edit list hides from
/// `ffprobe` but which `GStreamer` reports verbatim. Seeking to `T` therefore
/// lands on a picture stamped `T + base`, and that is *correct* — the frame
/// really is T into the stream.
///
/// Measured from the clip rather than hardcoded, so the seek tests calibrate
/// themselves against the stream's own timebase instead of encoding a magic
/// number that would quietly mask a genuine drift.
#[cfg(feature = "decode")]
pub fn stream_pts_base_ms(engine: &mut media::MediaEngine) -> u64 {
    let first = decode_frame_at(engine, 0, "stream timebase probe");
    assert!(
        first.pts_ms < 500,
        "the fixture's first picture is at {}ms; a base offset that large would \
         mask a real seek error rather than calibrate for one",
        first.pts_ms
    );
    first.pts_ms
}

/// How far a decoded picture landed from the time that was asked for.
///
/// Millisecond timestamps are unsigned and a drift is a difference between two
/// of them, so the magnitude comes from `abs_diff` and the direction is carried
/// alongside. Routing a PTS through `i64` to subtract would put a cast inside
/// the assertion itself, which is the one place a silent wrap must not be.
#[cfg(feature = "decode")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drift {
    /// Absolute distance in milliseconds.
    pub ms: u64,
    /// Whether the picture landed *before* the requested time.
    pub early: bool,
}

#[cfg(feature = "decode")]
impl Drift {
    /// The drift of `actual` from `target`.
    pub fn between(actual: u64, target: u64) -> Self {
        Self {
            ms: actual.abs_diff(target),
            early: actual < target,
        }
    }
}

#[cfg(feature = "decode")]
impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", if self.early { "-" } else { "+" }, self.ms)
    }
}

/// `part` as a percentage of `whole`, for a diagnostic line.
///
/// Byte counts are `usize`; widening them to `f64` here is exact for any
/// picture that fits in memory, and the result is printed, never asserted on.
#[cfg(feature = "decode")]
pub fn percent_of(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "byte counts of one decoded picture — far below 2^53, and the \
                  result only ever reaches a println"
    )]
    let pct = part as f64 / whole as f64 * 100.0;
    pct
}

/// Open the fixture, seek to `target_ms`, and pump until a picture lands.
///
/// Fails rather than returning `None` when nothing arrives: a decode test that
/// got no frame has proved nothing.
#[cfg(feature = "decode")]
pub fn decode_frame_at(
    engine: &mut media::MediaEngine,
    target_ms: u64,
    what: &str,
) -> media::Frame {
    let ok = engine.seek_and_wait(target_ms, std::time::Duration::from_secs(10));
    assert!(
        ok,
        "{what}: the pipeline produced no picture for {target_ms}ms within 10s. \
         Decoder: {}",
        engine.hw_report().describe()
    );
    engine
        .latest()
        .cloned()
        .unwrap_or_else(|| panic!("{what}: seek_and_wait reported success but no frame is held"))
}
