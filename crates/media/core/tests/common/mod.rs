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
//! for the fixture's codec is a legitimate configuration — the test still asserts, but it
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

#[cfg(feature = "decode")]
use gstreamer::prelude::*;

/// Frames per second of the fixture clip.
#[cfg(feature = "decode")]
pub const FIXTURE_FPS: f64 = 30.0;

/// Encoded picture dimensions exercised by the hardware/native-frame tests.
#[cfg(feature = "decode")]
pub const FIXTURE_WIDTH: u32 = 1_920;
#[cfg(feature = "decode")]
pub const FIXTURE_HEIGHT: u32 = 1_080;

/// One frame interval, milliseconds. A seek that lands within this of its
/// target landed on the right picture.
#[cfg(feature = "decode")]
pub const FRAME_MS: u64 = 33;

/// The fixture's keyframe interval. Verified by demuxing the generated clip:
/// keyframes sit at 0, 8.333, 16.667, 25, 33.333, 41.667 and 50 seconds and
/// nowhere else.
///
/// This is what makes the accurate-seek test sharp. A `KEY_UNIT` seek to a
/// time in the middle of a GOP cannot do better than the nearest of those, so
/// the difference between snapping and decoding to the target is measured in
/// **seconds**, not milliseconds.
#[cfg(feature = "decode")]
pub const FIXTURE_GOP_MS: u64 = 8_333;

/// Exactly 250 pictures separate keyframes at 30 fps.
#[cfg(feature = "decode")]
pub const FIXTURE_GOP_FRAMES: u32 = 250;

/// Number of pictures in the generated fixture.
#[cfg(feature = "decode")]
pub const FIXTURE_FRAMES: u32 = 1_717;

/// Caps of the fixture's video stream, for asking the registry which
/// decoders can take it.
#[cfg(feature = "decode")]
pub const FIXTURE_CAPS: &str = "video/x-vp9";

/// Human name of the fixture's codec, for messages.
#[cfg(feature = "decode")]
pub const FIXTURE_CODEC: &str = "VP9";

/// The fixture clip: 1080p VP9 in `WebM`, 30 fps, 57.2 s, 1717 frames.
///
/// VP9 is royalty-free and its encoder (libvpx) ships with every stack these
/// tests run on, so generating the subject never depends on a patented
/// encoder that a stock image leaves to the opt-in codec pack.
///
/// The clip is generated from an in-process `GStreamer` graph instead of living
/// in a developer's untracked artifact directory.  Its filename includes the
/// complete stream contract, so changing a dimension, cadence, frame count or
/// GOP necessarily creates a new cache entry rather than silently reusing an
/// incompatible file.
#[cfg(feature = "decode")]
pub fn fixture_path() -> PathBuf {
    static FIXTURE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(generate_fixture).clone()
}

/// Encode the exact seek/decode fixture once into the host's temporary cache.
///
/// Cargo launches each integration-test file as a separate process.  A stable
/// cache path lets all of those binaries share one encoding, while a
/// process-specific staging file makes publication atomic if they start at the
/// same time.  The source scrolls a detailed SMPTE image on every frame: it is
/// cheap to encode but guarantees that two correctly sought pictures differ.
#[cfg(feature = "decode")]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "failure to construct the real encoded test subject must fail the decode test loudly"
)]
fn generate_fixture() -> PathBuf {
    const CACHE_FILE: &str = "vp9-smpte-scroll-1920x1080-30fps-1717frames-gop250-v1.webm";

    let cache = std::env::temp_dir().join("tilo-media-test-fixtures");
    std::fs::create_dir_all(&cache).expect("create media fixture cache directory");
    let published = cache.join(CACHE_FILE);
    if fixture_is_ready(&published) {
        return published;
    }

    gstreamer::init().expect("initialize GStreamer for generated media fixture");
    for element in ["vp9enc", "webmmux"] {
        assert!(
            gstreamer::ElementFactory::find(element).is_some(),
            "the decode-test stack must provide {element} (gstreamer1-plugins-good) \
             so its VP9 subject can be generated"
        );
    }

    let staging = cache.join(format!("{CACHE_FILE}.{}.part", std::process::id()));
    std::fs::remove_file(&staging).ok();
    let description = format!(
        "videotestsrc pattern=smpte horizontal-speed=8 num-buffers={FIXTURE_FRAMES} ! \
         video/x-raw,format=I420,width={FIXTURE_WIDTH},height={FIXTURE_HEIGHT},\
         framerate={FIXTURE_FPS}/1 ! \
         vp9enc deadline=1 cpu-used=8 row-mt=true end-usage=cbr target-bitrate=4000000 \
         lag-in-frames=0 keyframe-mode=auto keyframe-max-dist={FIXTURE_GOP_FRAMES} ! \
         webmmux ! filesink name=fixture_sink"
    );
    let pipeline = gstreamer::parse::launch(&description)
        .expect("construct generated media fixture pipeline")
        .downcast::<gstreamer::Pipeline>()
        .expect("fixture graph is a pipeline");
    pipeline
        .by_name("fixture_sink")
        .expect("generated fixture filesink")
        .set_property("location", staging.to_string_lossy().as_ref());
    pipeline
        .set_state(gstreamer::State::Playing)
        .expect("start generated media fixture encoder");

    let bus = pipeline.bus().expect("generated fixture pipeline bus");
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(120))
        .expect("fixture deadline is representable");
    let mut reached_eos = false;
    while std::time::Instant::now() < deadline {
        let Some(message) = bus.timed_pop(gstreamer::ClockTime::from_mseconds(500)) else {
            continue;
        };
        match message.view() {
            gstreamer::MessageView::Eos(_) => {
                reached_eos = true;
                break;
            }
            gstreamer::MessageView::Error(error) => panic!(
                "generated media fixture failed in {}: {} ({:?})",
                error.src().map_or_else(
                    || "unknown element".into(),
                    gstreamer::prelude::GstObjectExt::path_string
                ),
                error.error(),
                error.debug(),
            ),
            _ => {}
        }
    }
    let _ = pipeline.set_state(gstreamer::State::Null);
    assert!(reached_eos, "generated media fixture timed out before EOS");
    assert!(
        fixture_is_ready(&staging),
        "generated fixture is not a real encoded WebM: {}",
        staging.display()
    );

    if let Err(error) = std::fs::rename(&staging, &published) {
        assert!(
            fixture_is_ready(&published),
            "publish generated media fixture {}: {error}",
            published.display()
        );
        std::fs::remove_file(&staging).ok();
    }
    published
}

#[cfg(feature = "decode")]
fn fixture_is_ready(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 512)
}

/// The fixture, failing loudly if it is not there. A decode test with no clip
/// has observed nothing and must not report success.
#[cfg(feature = "decode")]
pub fn require_fixture() -> PathBuf {
    let p = fixture_path();
    assert!(
        p.exists(),
        "decode fixture missing at {}. These tests verify real hardware decode \
         against a real {FIXTURE_CODEC} clip; without it nothing is being checked, so this \
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

/// Serialise tests that open engines under different [`media::HwPolicy`]s.
///
/// Decoder ranks live in the process-wide `GStreamer` registry, and an engine
/// applies its policy there when it opens a source. Two tests in one binary
/// racing each other would let one re-rank the registry while the other's
/// decoder is being autoplugged, so each holds this for its whole run.
#[cfg(feature = "decode")]
pub fn registry_policy_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    let capable = media::gst_backend::decoders_for(FIXTURE_CAPS);
    let list = |v: &[media::hw::DecoderEntry]| -> String {
        v.iter()
            .map(|d| format!("{}({}, rank {})", d.name, d.kind.as_str(), d.rank))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{} video decoders total; {FIXTURE_CODEC}-capable: [{}]",
        all.len(),
        list(&capable)
    )
}

/// Whether this machine has any hardware decoder that can take the fixture.
#[cfg(feature = "decode")]
pub fn has_hardware_fixture_decoder() -> bool {
    media::gst_backend::decoders_for(FIXTURE_CAPS)
        .iter()
        .any(|d| d.kind.is_hardware())
}

/// The presentation timestamp of the fixture's **first** decoded picture.
///
/// Measured from the encoded stream rather than assumed to be zero, so the seek
/// tests remain about drift instead of a muxer's choice of edit-list/timebase
/// representation.  The small upper bound prevents this calibration from ever
/// masking a real seek error.
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
