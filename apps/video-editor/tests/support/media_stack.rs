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
    const REQUIRED: [&str; 8] = [
        "openh264enc",
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
    if AAC
        .iter()
        .all(|name| gstreamer::ElementFactory::find(name).is_none())
    {
        missing.push("an AAC encoder");
    }
    missing.is_empty() || require_media(&format!("missing {}", missing.join(", ")))
}

fn require_media(problem: &str) -> bool {
    assert!(
        std::env::var_os("TILO_REQUIRE_MEDIA").is_none_or(|value| value != "1"),
        "TILO_REQUIRE_MEDIA=1 but the media stack is incomplete: {problem}"
    );
    eprintln!("footage not checked ({problem}); set TILO_REQUIRE_MEDIA=1 to make this a failure");
    false
}
