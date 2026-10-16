//! A decode build whose `GStreamer` finds no plugins plays nothing and says
//! why. It must never paint synthesized pictures in place of the file: a
//! sandboxed player whose plugin search missed the system directory once
//! "played" every video as a two-minute test pattern and never offered the
//! missing codec.

#[cfg(feature = "decode")]
#[test]
fn an_engine_without_plugins_fails_every_open_instead_of_painting_a_pattern() {
    use tilo_media::{MediaEngine, MediaEvent, SeekWaitError, Source};

    // This binary's only test, so `GStreamer` initialises here, against an
    // empty plugin search and a private registry.
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("GST_PLUGIN_SYSTEM_PATH_1_0", scratch.path());
    env.set("GST_PLUGIN_PATH_1_0", scratch.path());
    env.set("GST_REGISTRY_1_0", scratch.path().join("registry.bin"));
    env.set("XDG_CACHE_HOME", scratch.path());
    let clip = scratch.path().join("clip.mp4");
    std::fs::write(&clip, [0u8; 4096]).expect("a stand-in clip");

    let mut engine = MediaEngine::new();
    assert!(!engine.is_real_decode());
    for attempt in 0..2 {
        engine.open(Source::LocalFile(clip.clone()));
        let events = engine.poll();
        let reasons: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                MediaEvent::Error(reason) => Some(reason.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(reasons.len(), 1, "open {attempt}: {events:?}");
        assert!(
            reasons[0].contains("no playback element")
                && reasons[0].contains(&*scratch.path().to_string_lossy()),
            "the reason names the missing element and where plugins were looked for: {}",
            reasons[0]
        );
        assert!(engine.poll().is_empty(), "one failure per open");
        engine.play();
        assert!(!engine.is_playing());
        assert!(engine.latest_frame().is_none() && engine.latest().is_none());
        assert_eq!(engine.duration_ms(), None);
        assert_eq!(engine.position_ms(), 0);
    }
    assert!(matches!(
        engine.seek_and_wait(1_000, tilo_clock::Duration::from_secs(5)),
        Err(SeekWaitError::Failed(reason)) if reason.contains("no playback element")
    ));
}
