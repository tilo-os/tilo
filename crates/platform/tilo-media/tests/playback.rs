//! Proof that playback is driven by the pipeline clock, and that the audio
//! path is controllable.
//!
//! The editor's preview previously advanced a UI clock and seek-held the
//! pipeline: one flushing seek per drawn frame, each tearing down and refilling
//! the decoder, with a brief trip through `PLAYING` to force a picture out.
//! That produced no continuous decode, no A/V sync, and — because the audio
//! sink was started and stopped a few milliseconds at a time — an audible
//! stutter on every frame of a scrub.
//!
//! What replaces it: `play()` runs the pipeline, the pipeline's own clock
//! drives the playhead, and frames arrive paced to their timestamps. Scrubbing
//! stays in `PAUSED` and reads the preroll, so the audio sink is never
//! started at all.

mod common;

#[cfg(feature = "decode")]
mod real {
    use super::common::*;
    use std::time::{Duration, Instant};
    use tilo_media::{EngineConfig, MediaEngine};

    fn engine() -> MediaEngine {
        let mut e = MediaEngine::with_config(EngineConfig::default().silent());
        assert!(
            e.is_real_decode(),
            "these assertions require the real pipeline"
        );
        e.open(fixture_source());
        e
    }

    /// Pump for `dur`, returning every distinct frame timestamp seen.
    fn pump(e: &mut MediaEngine, dur: Duration) -> Vec<u64> {
        let mut seen: Vec<u64> = Vec::new();
        // `Instant + Duration` panics on overflow; `None` means no deadline is
        // representable, which for a pump loop is "keep pumping".
        let deadline = Instant::now().checked_add(dur);
        while deadline.is_none_or(|d| Instant::now() < d) {
            e.poll();
            if let Some(f) = e.latest()
                && seen.last() != Some(&f.pts_ms)
            {
                seen.push(f.pts_ms);
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        seen
    }

    #[test]
    fn playback_decodes_a_continuous_run_of_frames_rather_than_one_per_seek() {
        let mut e = engine();
        // Settle at a known position first.
        let start = decode_frame_at(&mut e, 5_000, "pre-roll before playing");
        e.play();
        assert!(e.is_playing());

        let seen = pump(&mut e, Duration::from_millis(1_200));
        println!("decoder: {}", e.hw_report().describe());
        println!(
            "{} distinct frames in 1.2s, first {:?} last {:?}",
            seen.len(),
            seen.first(),
            seen.last()
        );

        // At 30fps, a second of real playback is ~30 pictures. Requiring well
        // over a handful is what distinguishes continuous decode from the old
        // seek-per-frame behaviour, which produced one picture per request.
        assert!(
            seen.len() >= 15,
            "only {} frames arrived in 1.2s of playback; the pipeline is not \
             decoding continuously. Decoder: {}",
            seen.len(),
            e.hw_report().describe()
        );
        // Timestamps must advance monotonically from where playback began.
        assert!(
            seen.windows(2).all(|w| w[1] > w[0]),
            "frame timestamps went backwards during playback: {seen:?}"
        );
        assert!(
            *seen.last().unwrap() > start.pts_ms,
            "playback did not move past its starting picture"
        );
        e.pause();
    }

    #[test]
    fn the_playhead_follows_the_pipeline_clock_at_roughly_real_time() {
        let mut e = engine();
        let _ = decode_frame_at(&mut e, 5_000, "settle");
        let before = e.position_ms();
        e.play();

        let wall_start = Instant::now();
        pump(&mut e, Duration::from_millis(1_000));
        let elapsed = u64::try_from(wall_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let advanced = e.position_ms().saturating_sub(before);
        e.pause();

        println!("wall {elapsed}ms, playhead advanced {advanced}ms");
        // A pipeline-clock-driven playhead tracks wall time. Generous bounds:
        // this asserts "real time, not a stalled or free-running counter",
        // not stopwatch precision on a loaded machine.
        assert!(
            advanced > elapsed / 3,
            "the playhead advanced {advanced}ms over {elapsed}ms of playback — \
             it is barely moving, so playback is not clock-driven"
        );
        assert!(
            advanced < elapsed * 3,
            "the playhead advanced {advanced}ms over {elapsed}ms — it is running \
             far faster than the media clock, so nothing is pacing it"
        );
    }

    #[test]
    fn pausing_stops_the_playhead_and_resuming_continues_from_there() {
        let mut e = engine();
        let _ = decode_frame_at(&mut e, 5_000, "settle");
        e.play();
        pump(&mut e, Duration::from_millis(400));
        e.pause();
        assert!(!e.is_playing());

        let held = e.position_ms();
        std::thread::sleep(Duration::from_millis(300));
        e.poll();
        let after_idle = e.position_ms();
        assert!(
            after_idle.abs_diff(held) < 100,
            "a paused playhead drifted from {held}ms to {after_idle}ms"
        );

        e.play();
        pump(&mut e, Duration::from_millis(400));
        let resumed = e.position_ms();
        e.pause();
        assert!(
            resumed > held,
            "resuming did not advance the playhead: held {held}ms, resumed {resumed}ms"
        );
    }

    #[test]
    fn the_audio_chain_exists_and_its_gain_is_controllable() {
        // `prelude` brings `name()` in; the re-export is what lets a consumer
        // use the returned element at all without its own gstreamer dep.
        use tilo_media::gst::prelude::*;

        // The editor needs a place to attach per-source gain and monitoring.
        // Volume moves a dedicated `volume` element rather than playbin's own
        // property, so changing it does not re-negotiate the stream.
        let mut e = engine();
        let sink = e
            .audio_sink()
            .expect("a real audio sink must be built, not a fakesink");
        println!("audio sink: {}", sink.name());

        e.set_volume(0.25);
        assert!((e.volume() - 0.25).abs() < 1e-6);
        e.set_volume(2.0);
        assert!(
            (e.volume() - 1.0).abs() < 1e-6,
            "volume is clamped to the valid range, got {}",
            e.volume()
        );

        // Muting must not destroy the level it silences.
        e.set_volume(0.6);
        e.set_muted(true);
        assert!(e.is_muted());
        assert!(
            (e.volume() - 0.6).abs() < 1e-6,
            "mute preserves the volume setting"
        );
        e.set_muted(false);
        assert!(!e.is_muted());
        assert!((e.volume() - 0.6).abs() < 1e-6);
    }

    #[test]
    fn a_scrub_never_enters_playing_so_the_audio_sink_is_never_started() {
        // The stutter this replaces: the old preview seeked, entered PLAYING
        // to force a picture out, then paused — a few milliseconds of audio
        // per drawn frame, up to sixty times a second.
        let mut e = engine();
        for t in [1_000u64, 9_500, 22_300, 9_500, 40_000, 3_200] {
            let f = decode_frame_at(&mut e, t, "scrub");
            assert!(
                !e.is_playing(),
                "scrubbing to {t}ms entered playback (frame {}ms)",
                f.pts_ms
            );
        }
        // And a scrub after real playback must return to the paused path.
        e.play();
        pump(&mut e, Duration::from_millis(200));
        e.pause();
        let _ = decode_frame_at(&mut e, 15_000, "scrub after playback");
        assert!(!e.is_playing(), "the scrub left the pipeline running");
    }
}

/// The synthesized fallback has no pipeline clock, but its playhead must still
/// advance while playing and hold while paused.
#[cfg(not(feature = "decode"))]
mod hermetic {
    use tilo_media::MediaEngine;

    #[test]
    fn the_fallback_playhead_advances_while_playing_and_holds_while_paused() {
        let mut e = MediaEngine::new_synthesized();
        e.open(tilo_media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        e.play();
        e.poll_at(0);
        e.poll_at(1_000);
        assert_eq!(e.position_ms(), 1_000);
        e.pause();
        e.poll_at(2_000);
        assert_eq!(e.position_ms(), 1_000, "a paused playhead holds");
    }
}
