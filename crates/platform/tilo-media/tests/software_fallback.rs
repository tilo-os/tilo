//! Proof that the software path still works when hardware decode is off.
//!
//! Hardware decode is a preference, not a requirement: a machine with no GPU
//! video engine, a driver that will not load, or a codec the hardware cannot
//! take must all still play. `EngineConfig::software_only()` demotes every
//! hardware decoder below the software ones, which is exactly the situation
//! those machines are in — so exercising it here exercises them.
//!
//! The policy is a property of the engine's configuration rather than a
//! process-wide environment read. Applying it still re-ranks the process-wide
//! registry, so every test here holds `common::registry_policy_lock` while
//! its engine autoplugs.

mod common;

#[cfg(feature = "decode")]
mod real {
    use super::common::*;
    use tilo_media::{DecoderKind, EngineConfig, FramePayload, MediaEngine};

    #[test]
    fn software_only_decodes_the_same_clip_through_a_cpu_decoder() {
        let _registry = registry_policy_lock();
        let mut e = MediaEngine::with_config(EngineConfig::default().software_only().silent());
        assert!(e.is_real_decode(), "this test needs the real pipeline");
        e.open(fixture_source());

        let frame = decode_frame_at(&mut e, 1_000, "software fallback");
        let report = e.hw_report();
        println!("decoder: {}", report.describe());
        println!("registry: {}", registry_summary());

        let element = report
            .decoder_element
            .clone()
            .expect("a picture was decoded, so an element must have been recorded");
        assert_eq!(
            report.decoder_kind,
            Some(DecoderKind::Software),
            "with hardware demoted, autoplug must reach a CPU decoder; it chose {element:?}"
        );
        assert!(
            !report.is_hardware(),
            "{element:?} was plugged under a software-only policy"
        );

        // And it must be a real picture, not an empty one.
        assert_eq!((frame.width, frame.height), (FIXTURE_WIDTH, FIXTURE_HEIGHT));
        let rgba = frame
            .to_rgba()
            .expect("the RGBA path yields a paintable buffer");
        assert_eq!(
            rgba.len(),
            FIXTURE_WIDTH as usize * FIXTURE_HEIGHT as usize * 4
        );
        assert!(
            rgba.chunks_exact(4)
                .any(|px| px[0] > 8 || px[1] > 8 || px[2] > 8),
            "the decoded picture is entirely black; nothing was actually decoded"
        );
    }

    #[test]
    fn the_software_path_seeks_just_as_exactly() {
        let _registry = registry_policy_lock();
        // Accurate seeking is a property of the seek flags, not of the
        // decoder, and must hold on the fallback too.
        let mut e = MediaEngine::with_config(EngineConfig::default().software_only().silent());
        e.open(fixture_source());
        // The fixture's video track starts at a non-zero PTS; measure that base
        // from the clip rather than assuming zero, so a genuine drift still fails.
        let base = stream_pts_base_ms(&mut e);
        let frame = decode_frame_at(&mut e, 12_500, "software accurate seek");
        let delta = Drift::between(frame.pts_ms, 12_500 + base);
        println!(
            "software seek to 12500ms (base {base}ms) landed at {}ms ({delta}ms off)",
            frame.pts_ms
        );
        assert!(
            delta.ms <= FRAME_MS,
            "the software path landed {delta}ms from the requested time"
        );
    }

    #[test]
    fn an_rgba_only_consumer_is_never_handed_a_gpu_buffer_it_cannot_paint() {
        let _registry = registry_policy_lock();
        // The compatibility contract every existing consumer relies on: with
        // the default acceptance, `latest_frame()` is always populated.
        let mut e = MediaEngine::new();
        assert_eq!(e.config().acceptance, tilo_media::FrameAcceptance::Rgba);
        e.open(fixture_source());
        let frame = decode_frame_at(&mut e, 5_000, "rgba compatibility");

        assert!(
            matches!(frame.payload, FramePayload::Rgba(_)),
            "an RGBA-only consumer received {:?}",
            frame.payload
        );
        assert!(
            e.latest_frame().is_some(),
            "the long-standing VideoFrame surface must stay populated"
        );
        let vf = e.latest_frame().unwrap();
        assert_eq!(vf.rgba.len(), vf.width as usize * vf.height as usize * 4);
    }
}

/// Without the decode feature the fallback *is* the whole engine. Assert it
/// behaves, rather than compiling this file to nothing.
#[cfg(not(feature = "decode"))]
mod hermetic {
    use tilo_media::{EngineConfig, MediaEngine};

    #[test]
    fn a_software_only_config_still_produces_frames_with_no_gstreamer_linked() {
        let mut e = MediaEngine::with_config(EngineConfig::default().software_only());
        e.open(tilo_media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        assert!(e.latest_frame().is_some());
        assert!(!e.hw_report().is_hardware());
    }
}
