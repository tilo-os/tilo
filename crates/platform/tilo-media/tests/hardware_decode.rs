//! Proof that hardware decode actually engages — by name, not by assertion.
//!
//! Run against the generated VP9 fixture. The test interrogates the
//! `GStreamer` registry first, so its verdict is specific to the machine:
//!
//! * a hardware VP9 decoder exists → it **must** be the element that got
//!   plugged, or the test fails;
//! * no hardware VP9 decoder exists → the test proves that from the registry
//!   (and prints it), then still requires that decode *worked* through a
//!   software decoder.
//!
//! Neither branch is a skip. The second one is a real configuration — a build
//! container without a GPU passed through, or `libcuda.so.1` not mounted (so
//! `nvcodec` exposes zero features). On a host with `vavp9dec` or `nvvp9dec`
//! the first branch applies.
//!
//! `h264_decodes_through_the_codec_block_where_the_machine_has_one` is the same
//! proof for H.264, the codec a board's own codec block exists to decode. Its
//! subject is generated where an H.264 encoder is installed and is a failure
//! where none is, because the question only has an answer on a machine that
//! can pose it.

mod common;

#[cfg(feature = "decode")]
mod real {
    use super::common::*;
    use tilo_media::{DecoderKind, EngineConfig, FramePayload, MediaEngine};

    /// The decoder `GStreamer` chose for the fixture must be a hardware one
    /// whenever this machine has a hardware decoder that can take the fixture.
    #[test]
    fn the_decoder_that_ran_is_the_hardware_one_when_the_machine_has_one() {
        let _registry = registry_policy_lock();
        let summary = registry_summary();
        println!("registry: {summary}");

        let mut engine = MediaEngine::with_config(EngineConfig::gpu().frames_only());
        assert!(
            engine.is_real_decode(),
            "the decode feature is compiled but the GStreamer backend did not \
             come up; nothing below would be testing a real pipeline. {summary}"
        );
        engine.open(fixture_source());

        // A frame has to land before the report means anything: the decoder is
        // only plugged once the pipeline prerolls.
        let frame = decode_frame_at(&mut engine, 1_000, "hardware decode probe");
        assert_eq!(frame.width, FIXTURE_WIDTH);
        assert_eq!(frame.height, FIXTURE_HEIGHT);

        let report = engine.hw_report();
        let element = report.decoder_element.clone().unwrap_or_else(|| {
            panic!(
                "a {FIXTURE_CODEC} picture was decoded but no decoder element was \
                 recorded — the observation path is broken, so any claim about \
                 hardware decode would be unfounded. {summary}"
            )
        });
        println!("decoder: {}", report.describe());
        println!("payload: {:?}", frame.payload);

        if has_hardware_fixture_decoder() {
            assert!(
                report.is_hardware(),
                "this machine has a hardware {FIXTURE_CODEC} decoder but the pipeline \
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
                "no hardware {FIXTURE_CODEC} decoder is registered, yet {element:?} was \
                 classified as hardware — the classifier is wrong. {summary}"
            );
            println!(
                "no hardware {FIXTURE_CODEC} decoder on this machine; \
                 software decode via {element} verified instead"
            );
        }
    }

    /// Whatever decoder ran, the pipeline must have negotiated a memory type
    /// that is *not* the old forced system-memory RGBA — the whole point of
    /// the GPU-native acceptance mode.
    #[test]
    fn a_gpu_native_consumer_is_not_handed_forced_rgba() {
        let _registry = registry_policy_lock();
        let mut engine = MediaEngine::with_config(EngineConfig::gpu().frames_only());
        engine.open(fixture_source());
        let frame = decode_frame_at(&mut engine, 2_000, "native negotiation");

        let report = engine.hw_report();
        let caps = report.negotiated_caps.clone().unwrap_or_else(|| {
            panic!(
                "a frame arrived but its caps were not recorded: {}",
                report.describe()
            )
        });
        println!("negotiated: {caps}");

        match &frame.payload {
            FramePayload::Dmabuf(d) => {
                println!("zero-copy dmabuf: {}", d.describe());
                assert!(
                    report.is_dmabuf(),
                    "a dmabuf payload must show in the caps: {caps}"
                );
                assert!(
                    !d.planes.is_empty(),
                    "a dmabuf frame with no planes is unusable"
                );
            }
            FramePayload::Planar(p) => {
                // The hardware path on a decoder with no dmabuf export, and
                // still a real win: NV12 is 1.5 bytes per pixel against RGBA's
                // 4, and no colour conversion has happened on the CPU.
                println!("planar {:?}, {} planes", p.format, p.planes.len());
                assert_eq!(
                    p.planes.len(),
                    2,
                    "NV12/P010 carry a luma and a chroma plane"
                );
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
        let engine = MediaEngine::with_config(EngineConfig::gpu().frames_only());
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

    /// A board whose `SoC` carries an H.264 codec block decodes H.264 through
    /// it — or this test names the software decoder that ran instead.
    ///
    /// H.264 is the codec phone and camera footage arrives in, and on a laptop
    /// whose GPU has no VA-API the `SoC`'s own block is the only fixed-function
    /// decoder there is. So the subject is H.264, and the observation is the
    /// same one every hardware claim in this file rests on: the element
    /// `GStreamer` really plugged, read back from the running pipeline.
    ///
    /// Both branches assert. A machine with an H.264 hardware decoder must
    /// have plugged it, and one that has none must prove that from the
    /// registry and then show the CPU decoded the clip. The one outcome that
    /// is neither — no H.264 encoder on this machine, so the clip cannot
    /// exist — fails with that reason: a check that cannot observe its subject
    /// must fail, never report a decode it did not perform.
    #[test]
    fn h264_decodes_through_the_codec_block_where_the_machine_has_one() {
        let clip = h264_fixture().unwrap_or_else(|reason| {
            panic!(
                "{reason}. This test's subject is an H.264 clip, so it has nothing to \
                 assert without one"
            )
        });
        let capable = tilo_media::gst_backend::decoders_for(H264_CAPS);
        let listed = capable
            .iter()
            .map(|d| format!("{}({}, rank {})", d.name, d.kind.as_str(), d.rank))
            .collect::<Vec<_>>()
            .join(", ");
        println!("H.264-capable decoders: [{listed}]");

        let _registry = registry_policy_lock();
        let mut engine = MediaEngine::with_config(EngineConfig::gpu().frames_only());
        assert!(
            engine.is_real_decode(),
            "the decode feature is compiled but the GStreamer backend did not come up; \
             nothing below would be testing a real pipeline"
        );
        engine.open(tilo_media::Source::LocalFile(clip));
        let frame = decode_frame_at(&mut engine, 0, "H.264 hardware decode probe");
        assert_eq!(frame.width, FIXTURE_WIDTH);
        assert_eq!(frame.height, FIXTURE_HEIGHT);

        let report = engine.hw_report();
        let element = report.decoder_element.clone().unwrap_or_else(|| {
            panic!(
                "an H.264 picture was decoded but no decoder element was recorded, so any \
                 claim about hardware decode would be unfounded"
            )
        });
        println!("H.264 decoder: {} ({element})", report.describe());

        if capable.iter().any(|d| d.kind.is_hardware()) {
            assert!(
                report.is_hardware(),
                "this machine has an H.264 hardware decoder but the pipeline plugged \
                 {element:?} ({:?}). Hardware decode is not engaging. [{listed}]",
                report.decoder_kind
            );
        } else {
            // Proven from the registry rather than assumed: there is genuinely
            // no hardware H.264 decoder here, so the CPU one is the answer.
            assert_eq!(
                report.decoder_kind,
                Some(DecoderKind::Software),
                "no H.264 hardware decoder is registered, yet {element:?} was classified \
                 as {:?}. [{listed}]",
                report.decoder_kind
            );
            println!("no H.264 hardware decoder on this machine; {element} decoded instead");
        }
    }

    /// When a decoder that exports dmabufs is available, the frame it produces
    /// must be described completely enough to import: the right number of
    /// plane fds, each with a stride, plus the fourcc and modifier.
    ///
    /// This is the only test that exercises the dmabuf description path, and
    /// it earns its place — `DMA_DRM` caps carry a placeholder format with
    /// **zero** planes, so reading the plane count straight off them yields a
    /// frame that looks valid and carries no buffers at all.
    #[test]
    fn a_dmabuf_frame_is_described_completely_enough_to_import() {
        use tilo_media::{DmabufImage, HwPolicy};
        let _registry = registry_policy_lock();

        let va_capable = tilo_media::gst_backend::decoders_for(FIXTURE_CAPS)
            .into_iter()
            .any(|d| d.kind == DecoderKind::Va);
        if !va_capable {
            // Proven from the registry, not assumed. Nothing on this machine
            // can produce a dmabuf for the fixture, so there is no description to
            // check — but say so loudly rather than passing in silence.
            println!(
                "no VA-API {FIXTURE_CODEC} decoder registered, so no dmabuf export path exists                  for this codec here: {}",
                registry_summary()
            );
            let all = tilo_media::gst_backend::available_video_decoders();
            assert!(
                !all.is_empty(),
                "the registry has no video decoders at all, so this check could not                  observe anything — that is a broken environment, not a pass"
            );
            return;
        }

        let mut e = MediaEngine::with_config(
            EngineConfig::gpu()
                .frames_only()
                .with_hw_policy(HwPolicy::Force(DecoderKind::Va)),
        );
        e.open(fixture_source());
        let frame = decode_frame_at(&mut e, 4_000, "dmabuf description");
        let report = e.hw_report();
        println!("decoder: {}", report.describe());
        println!("payload: {:?}", frame.payload);

        assert_eq!(
            report.decoder_kind,
            Some(DecoderKind::Va),
            "the VA decoder was forced but {:?} ran",
            report.decoder_element
        );

        let Some(img) = frame.dmabuf() else {
            // VA can be made to output system memory; that is a legitimate
            // negotiation result and not a failure of the description path.
            println!(
                "VA-API negotiated {:?} rather than a dmabuf;                  the description path is not exercised on this run",
                frame.payload
            );
            assert!(
                frame.planar().is_some(),
                "a hardware decoder produced neither a dmabuf nor planar output"
            );
            return;
        };

        let img: &DmabufImage = img;
        println!("dmabuf: {}", img.describe());
        assert_eq!((img.width, img.height), (FIXTURE_WIDTH, FIXTURE_HEIGHT));
        assert_eq!(
            img.fourcc,
            tilo_media::dmabuf::DRM_FORMAT_NV12,
            "{FIXTURE_CODEC} 4:2:0 decodes to NV12, got {}",
            tilo_media::dmabuf::fourcc_str(img.fourcc)
        );
        assert_eq!(
            img.planes.len(),
            2,
            "NV12 is a luma plus an interleaved chroma plane; {} plane(s) means the              DMA_DRM placeholder format was read instead of the resolved one",
            img.planes.len()
        );
        for (i, p) in img.planes.iter().enumerate() {
            assert!(
                p.stride >= img.width,
                "plane {i} stride {} is below the width",
                p.stride
            );
            assert!(
                tilo_media::dmabuf::dmabuf_size(&p.fd).unwrap_or(0) > 0,
                "plane {i}'s fd is not a sizeable buffer"
            );
        }
        assert!(
            report.is_dmabuf(),
            "the caps must agree that this is a dmabuf"
        );
    }

    /// One session serves both the preview and thumbnail-style requests, and
    /// a revisited time comes from the cache rather than a second decode.
    #[test]
    fn one_pipeline_serves_repeated_requests_for_the_same_source() {
        use tilo_media::SessionPool;
        let _registry = registry_policy_lock();

        let mut pool = SessionPool::for_editor();
        let src = fixture_source();
        let session = pool.session(&src);
        assert_eq!(pool.len(), 1);

        {
            let mut s = session.lock().unwrap();
            s.set_fps(FIXTURE_FPS);
            let a = s
                .frame_at(3_000, tilo_test_env::hang_bound())
                .expect("the fixture decodes at 3s");
            assert_eq!((a.width, a.height), (FIXTURE_WIDTH, FIXTURE_HEIGHT));
            let (hits_before, _) = s.cache_stats();

            // The same time again must not re-decode.
            let b = s
                .frame_at(3_000, tilo_test_env::hang_bound())
                .expect("cached");
            let (hits_after, _) = s.cache_stats();
            assert!(
                hits_after > hits_before,
                "a revisited time must come from the cache, not a second seek"
            );
            assert_eq!(a.pts_ms, b.pts_ms, "the same time yields the same picture");
            // Read the description out and release the session before printing:
            // every other holder of this pool entry blocks on that lock, and
            // formatting is not decode work.
            let decoder = s.hw_report().describe();
            drop(s);
            println!("decoder: {decoder}");
        }

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
    use tilo_media::MediaEngine;

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
        e.open(tilo_media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        assert!(
            e.latest_frame().is_some(),
            "the synthesized fallback still paints"
        );
    }
}
