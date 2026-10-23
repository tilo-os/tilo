// Whether the media stack the footage tests need is present, shared by every
// test that encodes, decodes or plays generated footage.

/// Whether this machine has the `GStreamer` elements the footage tests encode,
/// mux and parse with.
///
/// Where one is missing, a test that needs them says so and returns instead of
/// failing on a developer box without the full plugin set. With
/// `TILO_REQUIRE_MEDIA=1`, which CI sets because its builder image carries the
/// stack, the absence fails the test: a check that cannot observe its subject
/// must not pass there.
pub fn media_stack() -> bool {
    const REQUIRED: [&str; 7] = [
        "h264parse",
        "mp4mux",
        "qtdemux",
        "wavparse",
        "webmmux",
        "vp8enc",
        "opusenc",
    ];
    const AAC: [&str; 3] = ["fdkaacenc", "avenc_aac", "voaacenc"];
    if gstreamer::init().is_err() {
        return require_media("GStreamer did not initialise");
    }
    let mut missing: Vec<&str> = REQUIRED
        .into_iter()
        .filter(|name| gstreamer::ElementFactory::find(name).is_none())
        .collect();
    if h264_encoder().is_none() {
        missing.push("an H.264 encoder");
    }
    if AAC
        .iter()
        .all(|name| gstreamer::ElementFactory::find(name).is_none())
    {
        missing.push("an AAC encoder");
    }
    missing.is_empty() || require_media(&format!("missing {}", missing.join(", ")))
}

/// The H.264 encoders footage is written with, in preference order: each
/// with the property that takes its keyframe interval in frames, and the
/// settings that keep frames in presentation order as `OpenH264`'s Baseline
/// profile does (no B-frames), so a decoded frame's time is its source
/// frame's. Linux CI carries `OpenH264`; macOS has none (Cisco's binary only
/// ever reaches a machine as its owner's own download), and there the
/// system's encoders write the footage.
const H264_ENCODERS: [(&str, &str, &str); 4] = [
    ("openh264enc", "gop-size", ""),
    ("x264enc", "key-int-max", "bframes=0"),
    (
        "vtenc_h264_hw",
        "max-keyframe-interval",
        "allow-frame-reordering=false",
    ),
    (
        "vtenc_h264",
        "max-keyframe-interval",
        "allow-frame-reordering=false",
    ),
];

/// The `gst-launch` fragment of the first installed [`H264_ENCODERS`] entry,
/// writing a keyframe every `keyframe_interval` frames, or `None` where none
/// is installed. Call after [`media_stack`], which initialises `GStreamer`.
#[allow(
    dead_code,
    reason = "this file is included by several test binaries; not every one encodes H.264 itself"
)]
pub fn h264_encoder_with_keyframes_every(keyframe_interval: u32) -> Option<String> {
    h264_encoder().map(|(element, property, in_order)| {
        format!("{element} {property}={keyframe_interval} {in_order}")
    })
}

fn h264_encoder() -> Option<(&'static str, &'static str, &'static str)> {
    H264_ENCODERS
        .into_iter()
        .find(|(element, _, _)| gstreamer::ElementFactory::find(element).is_some())
}

fn require_media(problem: &str) -> bool {
    assert!(
        std::env::var_os("TILO_REQUIRE_MEDIA").is_none_or(|value| value != "1"),
        "TILO_REQUIRE_MEDIA=1 but the media stack is incomplete: {problem}"
    );
    eprintln!("footage not checked ({problem}); set TILO_REQUIRE_MEDIA=1 to make this a failure");
    false
}
