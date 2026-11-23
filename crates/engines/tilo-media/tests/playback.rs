//! Proof that playback is driven by the pipeline clock, and that the audio path is
//! controllable.
//!
//! `play()` runs the pipeline, whose own clock drives the playhead, and frames arrive
//! paced to their timestamps; scrubbing stays in `PAUSED` and reads the preroll, so the
//! audio sink is never started.

#[cfg(feature = "decode")]
mod real {
    use crate::common::*;
    use std::time::{Duration, Instant};
    use tilo_media::{EngineConfig, MediaEngine};

    fn engine() -> MediaEngine {
        shared_gst_registry();
        let mut e = MediaEngine::with_config(EngineConfig::default().silent());
        assert!(
            e.is_real_decode(),
            "these assertions require the real pipeline"
        );
        e.open(fixture_source());
        e
    }

    fn pump(e: &mut MediaEngine, dur: Duration) -> Vec<u64> {
        let mut seen: Vec<u64> = Vec::new();
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

    /// Pump until `done` holds of every distinct frame timestamp seen and the
    /// engine, returning those timestamps.
    fn pump_until(
        e: &mut MediaEngine,
        what: &str,
        mut done: impl FnMut(&[u64], &MediaEngine) -> bool,
    ) -> Vec<u64> {
        let mut seen: Vec<u64> = Vec::new();
        tilo_test_env::wait_until(what, || {
            e.poll();
            if let Some(f) = e.latest()
                && seen.last() != Some(&f.pts_ms)
            {
                seen.push(f.pts_ms);
            }
            done(&seen, e)
        });
        seen
    }

    #[test]
    fn playback_decodes_a_continuous_run_of_frames_rather_than_one_per_seek() {
        let _registry = registry_policy_lock();
        let mut e = engine();
        let start = decode_frame_at(&mut e, 5_000, "pre-roll before playing");
        e.play();
        assert!(e.is_playing());

        // Fifteen pictures with nothing asked for after `play()`: the old
        // seek-per-frame behaviour produced one picture per request, so it
        // would stop at the first.
        let seen = pump_until(
            &mut e,
            "fifteen pictures of continuous decode",
            |seen, _| seen.len() >= 15,
        );
        println!("decoder: {}", e.hw_report().describe());
        println!(
            "{} distinct frames, first {:?} last {:?}",
            seen.len(),
            seen.first(),
            seen.last()
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
        let _registry = registry_policy_lock();
        let mut e = engine();
        let _ = decode_frame_at(&mut e, 5_000, "settle");
        let before = e.position_ms();
        e.play();

        // The playhead moves (not a stalled counter)...
        let wall_start = Instant::now();
        pump_until(&mut e, "the playhead to advance half a second", |_, e| {
            e.position_ms().saturating_sub(before) >= 500
        });
        let elapsed = u64::try_from(wall_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let advanced = e.position_ms().saturating_sub(before);
        e.pause();

        println!("wall {elapsed}ms, playhead advanced {advanced}ms");
        // ...and no faster than the media clock: a pipeline clock runs at
        // wall time, and a loaded machine can only make the wall time longer.
        assert!(
            advanced < elapsed * 3,
            "the playhead advanced {advanced}ms over {elapsed}ms — it is running \
             far faster than the media clock, so nothing is pacing it"
        );
    }

    #[test]
    fn pausing_stops_the_playhead_and_resuming_continues_from_there() {
        let _registry = registry_policy_lock();
        let mut e = engine();
        let _ = decode_frame_at(&mut e, 5_000, "settle");
        e.play();
        let started = e.position_ms();
        pump_until(&mut e, "the playhead to advance", |_, e| {
            e.position_ms() > started
        });
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
        pump_until(&mut e, "the resumed playhead to advance", |_, e| {
            e.position_ms() > held
        });
        let resumed = e.position_ms();
        e.pause();
        assert!(
            resumed > held,
            "resuming did not advance the playhead: held {held}ms, resumed {resumed}ms"
        );
    }

    #[test]
    fn the_audio_chain_exists_and_its_gain_is_controllable() {
        use tilo_media::gst::prelude::*;

        let _registry = registry_policy_lock();
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
        let _registry = registry_policy_lock();
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

    /// The player's one decoded stream, played where the session's outputs name: a captured
    /// engine hands its sound to the host, which fans it out to each speaker. A second speaker
    /// that the person takes out stops receiving at once, and the first keeps playing.
    #[test]
    fn the_players_sound_reaches_every_speaker_and_a_taken_out_one_leaves_the_rest() {
        use self::sound::{
            Casts, Devices, Playback, Speakers, played, pump_sound, speaker, tone_fixture,
        };
        let _registry = registry_policy_lock();
        let clip = tone_fixture();
        let mut e = MediaEngine::with_config(EngineConfig::default().captured());
        assert!(
            e.is_real_decode(),
            "these assertions require the real pipeline"
        );
        e.open(tilo_media::Source::LocalFile(clip));
        e.play();

        let devices = std::sync::Arc::new(Devices::default());
        let mut bridge = Playback::new(Speakers(std::sync::Arc::clone(&devices)), Casts);
        let outputs = vec![
            speaker("outputs/1", "Built-in"),
            speaker("outputs/2", "Desk"),
        ];
        bridge.apply(&outputs).expect("both speakers start");

        // One pipeline decodes the sound; the bridge fans it to both speakers.
        tilo_test_env::wait_until("sound on both speakers", || {
            pump_sound(&e, &mut bridge);
            played(&devices, "Built-in") >= 4_096 && played(&devices, "Desk") >= 4_096
        });

        // Both play the video's sound, not silence: the same samples on each.
        let (built_in, desk) = {
            let played = devices.played.lock().unwrap();
            (played["Built-in"].clone(), played["Desk"].clone())
        };
        let shared = built_in.len().min(desk.len());
        assert_eq!(
            built_in[..shared],
            desk[..shared],
            "both speakers play the one stream"
        );
        assert!(
            built_in[..shared].iter().any(|&sample| sample != 0),
            "the speakers play the video's sound, not silence"
        );

        // The person takes the desk speaker out of the session: it stops receiving, and the
        // built-in one keeps playing without a gap of more than the piece in flight.
        let built_in = played(&devices, "Built-in");
        let desk = played(&devices, "Desk");
        bridge.apply(&outputs[..1]).expect("the desk speaker goes");
        tilo_test_env::wait_until("the built-in speaker keeps playing", || {
            pump_sound(&e, &mut bridge);
            played(&devices, "Built-in") >= built_in.saturating_add(4_096)
        });
        assert_eq!(
            played(&devices, "Desk"),
            desk,
            "a taken-out speaker receives no more sound"
        );
        e.pause();
    }

    /// The pieces the player's sound goes through: a bridge whose speakers record what they
    /// receive, and a tone made at test time (no media is committed).
    mod sound {
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        use gstreamer::prelude::*;
        use tilo_media::MediaEngine;
        use tilo_media::playback_bridge::{CollectionOutput, PlaybackBridge};
        use tilo_media::playback_cast::{CastEvent, CastTransport, Receiver};
        use tilo_media::playback_sink::{OpenSink, SinkFactory, SinkStream};

        use crate::common::shared_gst_registry;

        pub type Playback = PlaybackBridge<Speakers, Casts>;

        /// What each speaker received.
        #[derive(Default)]
        pub struct Devices {
            pub played: Mutex<BTreeMap<String, Vec<i16>>>,
        }

        pub struct Speakers(pub Arc<Devices>);

        impl SinkFactory for Speakers {
            fn open(&self, device: &str) -> Result<OpenSink, String> {
                Ok(OpenSink {
                    device: device.to_owned(),
                    stream: Box::new(Speaker {
                        chart: Arc::clone(&self.0),
                        device: device.to_owned(),
                    }),
                })
            }
        }

        struct Speaker {
            chart: Arc<Devices>,
            device: String,
        }

        impl SinkStream for Speaker {
            fn write(&mut self, samples: &[i16]) -> Result<(), String> {
                if let Ok(mut played) = self.chart.played.lock() {
                    played
                        .entry(self.device.clone())
                        .or_default()
                        .extend_from_slice(samples);
                }
                Ok(())
            }

            fn close(&mut self) {}
        }

        /// A cast side that finds nothing and casts nowhere: this test plays on speakers.
        #[derive(Default)]
        pub struct Casts;

        impl CastTransport for Casts {
            fn discover(&mut self) -> Result<Vec<Receiver>, String> {
                Ok(Vec::new())
            }

            fn begin(&mut self, _cast: &str, _receiver: &Receiver) -> Result<(), String> {
                Err("this test casts nowhere".to_owned())
            }

            fn end(&mut self, _cast: &str) {}

            fn events(&mut self) -> Vec<(String, CastEvent)> {
                Vec::new()
            }
        }

        pub fn speaker(id: &str, device: &str) -> CollectionOutput {
            CollectionOutput {
                id: id.to_owned(),
                kind: "speaker".to_owned(),
                device: device.to_owned(),
                name: device.to_owned(),
                volume: 1.0,
                synced: true,
            }
        }

        /// How many samples `device` received.
        pub fn played(devices: &Arc<Devices>, device: &str) -> usize {
            devices
                .played
                .lock()
                .map_or(0, |played| played.get(device).map_or(0, Vec::len))
        }

        /// Hand the player's decoded sound to the bridge, then wait for more.
        pub fn pump_sound(engine: &MediaEngine, bridge: &mut Playback) {
            let samples = engine.take_audio();
            if !samples.is_empty() {
                bridge.write(&samples);
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        /// A stereo 48 kHz WAV of a sine tone, made once: a source with sound and no picture.
        pub fn tone_fixture() -> PathBuf {
            let cache = std::env::var_os("TILO_TEST_CACHE_DIR")
                .filter(|dir| !dir.is_empty())
                .map_or_else(std::env::temp_dir, PathBuf::from)
                .join("tilo-media-test-fixtures");
            std::fs::create_dir_all(&cache).expect("create media fixture cache directory");
            let clip = cache.join("tone-v1.wav");
            if clip.is_file() {
                return clip;
            }
            shared_gst_registry();
            gstreamer::init().expect("initialize GStreamer for the tone fixture");
            let pipeline = gstreamer::parse::launch(
                "audiotestsrc wave=sine num-buffers=90 ! \
                 audio/x-raw,rate=48000,channels=2 ! wavenc ! filesink name=sound",
            )
            .expect("construct the tone fixture pipeline")
            .downcast::<gstreamer::Pipeline>()
            .expect("the tone fixture graph is a pipeline");
            pipeline
                .by_name("sound")
                .expect("the tone fixture's output file")
                .set_property("location", clip.to_string_lossy().as_ref());
            pipeline
                .set_state(gstreamer::State::Playing)
                .expect("start the tone fixture encoder");
            let bus = pipeline.bus().expect("the tone fixture pipeline bus");
            let deadline = tilo_test_env::hang_deadline();
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
                        "the tone fixture failed: {} ({:?})",
                        error.error(),
                        error.debug()
                    ),
                    _ => {}
                }
            }
            let _ = pipeline.set_state(gstreamer::State::Null);
            assert!(reached_eos, "the tone fixture timed out before EOS");
            assert!(
                std::fs::metadata(&clip).is_ok_and(|metadata| metadata.len() > 4_096),
                "the tone fixture is not a real WAV: {}",
                clip.display()
            );
            clip
        }
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
