//! Proof that hardware decode actually engages — by name, not by assertion.
//!
//! Run against the real 1080p H.264 fixture. The test interrogates the
//! GStreamer registry first, so its verdict is specific to the machine:
//!
//! * a hardware H.264 decoder exists → it **must** be the element that got
//!   plugged, or the test fails;
//! * no hardware H.264 decoder exists → the test proves that from the registry
//!   (and prints it), then still requires that decode *worked* through a
//!   software decoder.
//!
//! Neither branch is a skip. The second one is a real configuration — the qOS
//! build container has no H.264 hardware decoder, because `libcuda.so.1` is
//! not mounted into it (so `nvcodec` exposes zero features) and Fedora's Mesa
//! VA driver ships without H.264. On the host, where both `nvh264dec` and
//! `vah264dec` are present, the first branch applies.

mod common;

#[cfg(feature = "decode")]
mod real {
    use super::common::*;
    use media::{DecoderKind, EngineConfig, FramePayload, MediaEngine};

    /// The decoder GStreamer chose for the fixture must be a hardware one
    /// whenever this machine has a hardware decoder that can take H.264.
    #[test]
    fn the_decoder_that_ran_is_the_hardware_one_when_the_machine_has_one() {
        let summary = registry_summary();
        println!("registry: {summary}");

        let mut engine = MediaEngine::with_config(EngineConfig::gpu().silent());
        assert!(
            engine.is_real_decode(),
            "the decode feature is compiled but the GStreamer backend did not \
             come up; nothing below would be testing a real pipeline. {summary}"
        );
        engine.open(fixture_source());

        // A frame has to land before the report means anything: the decoder is
        // only plugged once the pipeline prerolls.
        let frame = decode_frame_at(&mut engine, 1_000, "hardware decode probe");
        assert_eq!(frame.width, 1920);
        assert_eq!(frame.height, 1080);

        let report = engine.hw_report();
        let element = report.decoder_element.clone().unwrap_or_else(|| {
            panic!(
                "a 1920x1080 picture was decoded but no decoder element was \
                 recorded — the observation path is broken, so any claim about \
                 hardware decode would be unfounded. {summary}"
            )
        });
        println!("decoder: {}", report.describe());
        println!("payload: {:?}", frame.payload);

        if has_hardware_h264() {
            assert!(
                report.is_hardware(),
                "this machine has a hardware H.264 decoder but the pipeline \
                 plugged {element:?} ({:?}), a software decoder. Hardware decode \
                 is not engaging. {summary}",
                report.decoder_kind
            );
            assert_ne!(report.decoder_kind, Some(DecoderKind::Software));
        } else {
            // Proven from the registry, not assumed: there is genuinely no
            // hardware decoder for this codec here.
            assert!(
                !report.is_hardware(),
                "no hardware H.264 decoder is registered, yet {element:?} was \
                 classified as hardware — the classifier is wrong. {summary}"
            );
            println!(
                "no hardware H.264 decoder on this machine; \
                 software decode via {element} verified instead"
            );
        }
    }

    /// Whatever decoder ran, the pipeline must have negotiated a memory type
    /// that is *not* the old forced system-memory RGBA — the whole point of
    /// the GPU-native acceptance mode.
    #[test]
    fn a_gpu_native_consumer_is_not_handed_forced_rgba() {
        let mut engine = MediaEngine::with_config(EngineConfig::gpu().silent());
        engine.open(fixture_source());
        let frame = decode_frame_at(&mut engine, 2_000, "native negotiation");

        let report = engine.hw_report();
        let caps = report.negotiated_caps.clone().unwrap_or_else(|| {
            panic!("a frame arrived but its caps were not recorded: {}", report.describe())
        });
        println!("negotiated: {caps}");

        match &frame.payload {
            FramePayload::Dmabuf(d) => {
                println!("zero-copy dmabuf: {}", d.describe());
                assert!(report.is_dmabuf(), "a dmabuf payload must show in the caps: {caps}");
                assert!(!d.planes.is_empty(), "a dmabuf frame with no planes is unusable");
            }
            FramePayload::Planar(p) => {
                // The hardware path on a decoder with no dmabuf export, and
                // still a real win: NV12 is 1.5 bytes per pixel against RGBA's
                // 4, and no colour conversion has happened on the CPU.
                println!("planar {:?}, {} planes", p.format, p.planes.len());
                assert_eq!(p.planes.len(), 2, "NV12/P010 carry a luma and a chroma plane");
                assert!(
                    caps.contains("NV12") || caps.contains("P010"),
                    "a planar payload must match the negotiated caps: {caps}"
                );
            }
            FramePayload::Rgba(_) => panic!(
                "a GPU-native consumer was still handed system-memory RGBA. \
                 Negotiation failed to reach dmabuf or planar output: {caps}"
            ),
        }
    }

    /// The engine must never claim hardware decode before it has evidence.
    #[test]
    fn no_hardware_claim_is_made_before_a_decoder_has_been_plugged() {
        let engine = MediaEngine::with_config(EngineConfig::gpu().silent());
        let report = engine.hw_report();
        assert!(
            report.decoder_element.is_none(),
            "nothing has been opened yet, so no decoder can have been chosen"
        );
        assert!(
            !report.is_hardware(),
            "'not yet known' must never read as 'yes, hardware'"
        );
        assert!(!report.is_dmabuf());
    }

    /// One session serves both the preview and thumbnail-style requests, and
    /// a revisited time comes from the cache rather than a second decode.
    #[test]
    fn one_pipeline_serves_repeated_requests_for_the_same_source() {
        use media::SessionPool;

        let mut pool = SessionPool::for_editor();
        let src = fixture_source();
        let session = pool.session(&src);
        assert_eq!(pool.len(), 1);

        {
            let mut s = session.lock().unwrap();
            s.set_fps(FIXTURE_FPS);
            let a = s
                .frame_at(3_000, std::time::Duration::from_secs(10))
                .expect("the fixture decodes at 3s");
            assert_eq!((a.width, a.height), (1920, 1080));
            let (hits_before, _) = s.cache_stats();

            // The same time again must not re-decode.
            let b = s
                .frame_at(3_000, std::time::Duration::from_secs(10))
                .expect("cached");
            let (hits_after, _) = s.cache_stats();
            assert!(
                hits_after > hits_before,
                "a revisited time must come from the cache, not a second seek"
            );
            assert_eq!(a.pts_ms, b.pts_ms, "the same time yields the same picture");
            println!("decoder: {}", s.hw_report().describe());
        }

        // A second request for the same source reuses the pipeline.
        let again = pool.session(&src);
        assert!(
            std::sync::Arc::ptr_eq(&session, &again),
            "a thumbnail request must reuse the preview's pipeline"
        );
        assert_eq!(pool.len(), 1, "no second engine was constructed");
    }
}

/// Without the decode feature there is no pipeline to interrogate, and this
/// build's contract is that the synthesized fallback keeps working. Assert
/// that contract rather than compiling to nothing.
#[cfg(not(feature = "decode"))]
mod hermetic {
    use media::MediaEngine;

    #[test]
    fn the_hermetic_build_reports_no_real_decode_and_still_produces_frames() {
        assert!(
            !MediaEngine::is_decode_compiled(),
            "this module only compiles without the decode feature"
        );
        let mut e = MediaEngine::new();
        assert!(!e.is_real_decode(), "no GStreamer is linked in this build");
        assert!(
            !e.hw_report().is_hardware(),
            "a build with no decoder must never report hardware decode"
        );
        e.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        assert!(e.latest_frame().is_some(), "the synthesized fallback still paints");
    }
}
