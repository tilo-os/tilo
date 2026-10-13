// Deterministic encoded footage shared by the video editor's fidelity tests.
//
// Keep this generated in-process: the tests need a real, seekable compressed
// stream, but an untracked developer recording is neither reproducible nor
// available to open-source CI.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

pub const DURATION_SECS: f64 = 4.0;
pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 180;
pub const FPS: u32 = 30;
const FRAMES: u32 = 4 * FPS;
/// Part of the cached file's name: change it whenever the pictures or the
/// sound the generator produces change, so no build reuses a stale clip.
const GENERATION: u32 = 1;

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/media_stack.rs"
));

/// Return a real H.264/AAC MP4 source with deterministic moving imagery and
/// an audible 440 Hz stereo track.
///
/// The clip is encoded once per build, not per test process: it is cached in
/// the target directory beside the test binaries, where `cargo clean` removes
/// it. `TILO_VE_TEST_CLIP` remains useful for deliberately exercising another
/// source, but normal tests have no dependency outside the checkout and the
/// `GStreamer` stack they already test.
pub fn encoded_video() -> std::path::PathBuf {
    static CLIP: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    if let Some(path) = std::env::var_os("TILO_VE_TEST_CLIP") {
        let path = std::path::PathBuf::from(path);
        assert!(
            path.exists(),
            "$TILO_VE_TEST_CLIP does not exist: {}",
            path.display()
        );
        return path;
    }

    CLIP.get_or_init(|| encode_fixture(true)).clone()
}

/// A matching encoded moving-picture source with deliberately no audio pad.
/// Only negative tests that must prove absence of a stream should use this.
#[allow(
    dead_code,
    reason = "the shared helper is included by several test binaries; only export's negative test needs it"
)]
pub fn encoded_video_only() -> std::path::PathBuf {
    static CLIP: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    CLIP.get_or_init(|| encode_fixture(false)).clone()
}

fn encode_fixture(with_audio: bool) -> std::path::PathBuf {
    // Test binaries live in `target/<profile>/deps`; keep the clip next to
    // that profile's other outputs.
    let cache = std::env::current_exe()
        .expect("locate the running test binary")
        .parent()
        .and_then(std::path::Path::parent)
        .expect("the test binary sits in a profile directory")
        .join("video-editor-fixtures");
    std::fs::create_dir_all(&cache).expect("create the generated clip cache");
    let path = cache.join(format!(
        "motion-g{GENERATION}-{DURATION_SECS}s-{WIDTH}x{HEIGHT}-{FPS}fps-{}.mp4",
        if with_audio { "av" } else { "video-only" },
    ));
    if path.exists() {
        return path;
    }
    // Every test runs in its own process and they start together: the first
    // to take the lock encodes, the rest wait for it and reuse its clip.
    let _lock =
        advisory_lock::FileLock::open(&cache.join(".lock"), advisory_lock::LockMode::Exclusive)
            .expect("lock the generated clip cache");
    if path.exists() {
        return path;
    }
    // Encode beside the final name and rename into place, so a test process
    // that checks before taking the lock sees either no clip or a complete one.
    let partial = cache.join(format!(
        "{}.{}.partial",
        path.file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 fixture name"),
        std::process::id()
    ));
    encode_to(&partial, with_audio);
    std::fs::rename(&partial, &path).expect("publish the generated clip");
    path
}

fn encode_to(path: &std::path::Path, with_audio: bool) {
    gst::init().expect("initialize GStreamer for generated video fixture");
    std::fs::remove_file(path).ok();

    let audio_branch = if with_audio {
        let audio_encoder = ["fdkaacenc", "avenc_aac", "voaacenc"]
            .into_iter()
            .find(|name| gst::ElementFactory::find(name).is_some())
            .expect("the export test stack must provide an AAC encoder");
        format!(
            "audiotestsrc wave=sine freq=440 volume=0.8 samplesperbuffer=1600 \
                 num-buffers={FRAMES} ! audioconvert ! audioresample ! \
                 audio/x-raw,channels=2,rate=48000 ! \
                 {audio_encoder} ! aacparse ! queue ! mux."
        )
    } else {
        String::new()
    };
    let description = format!(
        "mp4mux name=mux ! filesink location={} \
             appsrc name=fixture is-live=false block=true max-bytes=4194304 format=time \
             caps=video/x-raw,format=RGBA,width={WIDTH},height={HEIGHT},framerate={FPS}/1 ! \
             videoconvert ! openh264enc gop-size={FPS} ! h264parse ! queue ! mux. \
             {audio_branch}",
        path.display(),
    );
    let pipeline = gst::parse::launch(&description)
        .expect("construct generated video fixture pipeline")
        .downcast::<gst::Pipeline>()
        .expect("fixture graph is a pipeline");
    pipeline
        .set_state(gst::State::Playing)
        .expect("start generated video fixture encoder");

    let source = pipeline
        .by_name("fixture")
        .expect("generated fixture appsrc")
        .downcast::<gst_app::AppSrc>()
        .expect("fixture source is an appsrc");
    push_video_frames(&source);
    source
        .end_of_stream()
        .expect("finish generated video stream");

    let bus = pipeline.bus().expect("fixture pipeline has a bus");
    let deadline = tilo_test_env::hang_deadline();
    let mut reached_eos = false;
    while ui::time::now() < deadline {
        let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(500)) else {
            continue;
        };
        match message.view() {
            gst::MessageView::Eos(_) => {
                reached_eos = true;
                break;
            }
            gst::MessageView::Error(error) => panic!(
                "generated video fixture failed in {}: {} ({:?})",
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
    let _ = pipeline.set_state(gst::State::Null);
    assert!(reached_eos, "generated video fixture timed out before EOS");
    assert!(
        std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 512),
        "generated fixture is not a real encoded MP4: {}",
        path.display()
    );
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "test-fixture dimensions are fixed; panics make a missing codec or malformed buffer fail loudly"
)]
fn push_video_frames(source: &gst_app::AppSrc) {
    let frame_duration = 1_000_000_000 / u64::from(FPS);
    let byte_len = usize::try_from(WIDTH * HEIGHT * 4).expect("fixture dimensions fit usize");
    let mut rgba = vec![255u8; byte_len];
    for frame in 0..u64::from(FRAMES) {
        // A scrolling low-luma stripe field provides stable fine detail
        // while the orange block supplies non-periodic local motion. The
        // deliberately dark average also makes inversion, brightness and
        // wipe assertions visually decisive rather than nearly neutral.
        let frame_u32 = u32::try_from(frame).expect("fixture frame count fits u32");
        let phase = frame_u32 * 2;
        let block_x = i64::try_from(frame * 3 % (u64::from(WIDTH) + 64))
            .expect("fixture width fits i64")
            - 64;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let stripe = ((x + phase) / 16).is_multiple_of(2);
                let in_block = i64::from(x) >= block_x
                    && i64::from(x) < block_x + 64
                    && (56..124).contains(&y);
                let colour = if in_block {
                    [230, 118, 24]
                } else if stripe {
                    [24, 50, 78]
                } else {
                    [76, 38, 24]
                };
                let offset =
                    usize::try_from((y * WIDTH + x) * 4).expect("fixture pixel offset fits usize");
                rgba[offset..offset + 3].copy_from_slice(&colour);
            }
        }
        let mut buffer = gst::Buffer::with_size(rgba.len()).expect("allocate fixture frame");
        {
            let buffer = buffer.get_mut().expect("new fixture buffer is unique");
            buffer.set_pts(gst::ClockTime::from_nseconds(frame * frame_duration));
            buffer.set_duration(gst::ClockTime::from_nseconds(frame_duration));
            buffer
                .map_writable()
                .expect("map fixture frame")
                .copy_from_slice(&rgba);
        }
        source
            .push_buffer(buffer)
            .unwrap_or_else(|error| panic!("push fixture frame {frame}: {error:?}"));
    }
}
