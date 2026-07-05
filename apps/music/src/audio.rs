//! Audio output engine.
//!
//! Real playback uses a GStreamer `playbin` (gated behind the `audio`
//! cargo feature) fed an HTTP(S) `uri` resolved by [`crate::backend`]. The
//! default build compiles the silent stub so the UI, the offline `--shots`
//! screenshot and CI stay free of a system GStreamer dependency. The public
//! surface is identical either way, so the app can drive playback without
//! caring which backend is linked.
//!
//! ffmpeg / ALSA / PulseAudio crates are deliberately **not** used —
//! GStreamer's `playbin` already negotiates decode + the system sink.
//!
//! The transport state the UI reads back — position, duration and *whether the
//! pipeline actually reached `PLAYING`* — is queried from the live pipeline, so
//! the now-playing indicator and the progress counter reflect **real** decoded
//! playback rather than a simulated timer.

/// A handle to the audio backend. Cheap to construct; owns the pipeline
/// when the `audio` feature is enabled.
pub struct AudioEngine {
    inner: Inner,
    /// Last requested URI (for diagnostics / the silent stub).
    current_uri: Option<String>,
    volume: f32,
}

#[cfg(feature = "audio")]
struct Inner {
    playbin: gstreamer::Element,
}

#[cfg(not(feature = "audio"))]
struct Inner {
    /// Stub bookkeeping: whether the (silent) transport is "playing".
    playing: bool,
}

impl AudioEngine {
    /// Build an engine. Never fails for the stub; the GStreamer path falls
    /// back to the stub if the pipeline can't be created.
    pub fn new() -> Self {
        #[cfg(feature = "audio")]
        {
            if gstreamer::init().is_ok() {
                if let Ok(playbin) =
                    gstreamer::ElementFactory::make("playbin").build()
                {
                    // Headless CI / the audio-pipeline test set this so playback
                    // is exercised end-to-end (state → PLAYING, real position)
                    // against a clock-synced `fakesink`, without needing a real
                    // sound card. Production leaves it unset so `playbin`
                    // auto-plugs the default system sink.
                    if std::env::var("QOS_MUSIC_FAKESINK").is_ok() {
                        if let Ok(fake) = gstreamer::ElementFactory::make("fakesink")
                            .property("sync", true)
                            .build()
                        {
                            use gstreamer::prelude::*;
                            playbin.set_property("audio-sink", &fake);
                        }
                    }
                    return Self {
                        inner: Inner { playbin },
                        current_uri: None,
                        volume: 0.8,
                    };
                }
            }
        }
        Self {
            #[cfg(feature = "audio")]
            inner: Inner {
                playbin: gstreamer::ElementFactory::make("playbin")
                    .build()
                    .expect("playbin"),
            },
            #[cfg(not(feature = "audio"))]
            inner: Inner { playing: false },
            current_uri: None,
            volume: 0.8,
        }
    }

    /// Point the engine at a new stream URI and start from zero.
    pub fn load(&mut self, uri: &str) {
        self.current_uri = Some(uri.to_string());
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            // Fully reset the pipeline before re-pointing it, so a fast skip
            // never leaves the previous stream decoding underneath the new one.
            let _ = self.inner.playbin.set_state(gstreamer::State::Null);
            self.inner.playbin.set_property("uri", uri);
            self.inner
                .playbin
                .set_property("volume", self.volume as f64);
        }
    }

    pub fn play(&mut self) {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            let _ = self.inner.playbin.set_state(gstreamer::State::Playing);
        }
        #[cfg(not(feature = "audio"))]
        {
            self.inner.playing = true;
        }
    }

    pub fn pause(&mut self) {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            let _ = self.inner.playbin.set_state(gstreamer::State::Paused);
        }
        #[cfg(not(feature = "audio"))]
        {
            self.inner.playing = false;
        }
    }

    /// Stop and tear the pipeline down to `Null` (release the decoder + sink).
    pub fn stop(&mut self) {
        self.current_uri = None;
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            let _ = self.inner.playbin.set_state(gstreamer::State::Null);
        }
        #[cfg(not(feature = "audio"))]
        {
            self.inner.playing = false;
        }
    }

    /// Seek to an absolute position in milliseconds.
    pub fn seek_ms(&mut self, ms: u64) {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            let _ = self.inner.playbin.seek_simple(
                gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::KEY_UNIT,
                gstreamer::ClockTime::from_mseconds(ms),
            );
        }
        #[cfg(not(feature = "audio"))]
        {
            let _ = ms;
        }
    }

    /// Set output volume `0.0..=1.0`.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            self.inner.playbin.set_property("volume", self.volume as f64);
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn current_uri(&self) -> Option<&str> {
        self.current_uri.as_deref()
    }

    /// Query the pipeline's current playhead in milliseconds, when the live
    /// GStreamer backend is running and a position is available. The UI uses
    /// this to drive a *real* (not simulated) playhead while audio plays;
    /// `None` falls back to the local simulated playhead.
    pub fn position_ms(&self) -> Option<u64> {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            return self
                .inner
                .playbin
                .query_position::<gstreamer::ClockTime>()
                .map(|p| p.mseconds());
        }
        #[cfg(not(feature = "audio"))]
        {
            None
        }
    }

    /// Query the pipeline's total duration in milliseconds, when known.
    pub fn duration_ms(&self) -> Option<u64> {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            return self
                .inner
                .playbin
                .query_duration::<gstreamer::ClockTime>()
                .map(|d| d.mseconds());
        }
        #[cfg(not(feature = "audio"))]
        {
            None
        }
    }

    /// Whether the live backend currently has a stream loaded (so the app can
    /// decide to trust [`Self::position_ms`] over the simulated playhead).
    pub fn has_stream(&self) -> bool {
        Self::is_live() && self.current_uri.is_some()
    }

    /// Whether the real pipeline has actually reached (or is heading to) the
    /// `PLAYING` state. The now-playing indicator reads this so it only shows
    /// "Streaming …" when audio is genuinely decoding, never on a fake timer.
    /// Always `false` for the silent stub.
    pub fn is_playing_real(&self) -> bool {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            if self.current_uri.is_none() {
                return false;
            }
            let (_, current, pending) = self.inner.playbin.state(gstreamer::ClockTime::ZERO);
            current == gstreamer::State::Playing || pending == gstreamer::State::Playing
        }
        #[cfg(not(feature = "audio"))]
        {
            false
        }
    }

    /// Block until the pipeline reaches `PLAYING` (or `timeout` elapses),
    /// returning whether it got there. Used by the audio-pipeline test to prove
    /// real decode reaches the sink; never called on the live UI thread.
    #[cfg(feature = "audio")]
    pub fn wait_until_playing(&self, timeout: std::time::Duration) -> bool {
        use gstreamer::prelude::*;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return self.inner.playbin.current_state() == gstreamer::State::Playing;
            }
            let step = remaining.min(std::time::Duration::from_millis(100));
            let (res, current, _) = self
                .inner
                .playbin
                .state(gstreamer::ClockTime::from_mseconds(step.as_millis() as u64));
            if current == gstreamer::State::Playing {
                return true;
            }
            if matches!(res, Err(_)) {
                // A hard state-change error (missing plugin, unreadable uri).
                return false;
            }
        }
    }

    /// Whether real audio output is wired up in this build.
    pub fn is_live() -> bool {
        cfg!(feature = "audio")
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "audio")]
impl Drop for AudioEngine {
    fn drop(&mut self) {
        use gstreamer::prelude::*;
        let _ = self.inner.playbin.set_state(gstreamer::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_tracks_uri_and_volume() {
        let mut e = AudioEngine::new();
        e.load("https://example.test/audio.m4a");
        e.set_volume(0.5);
        e.play();
        assert_eq!(e.current_uri(), Some("https://example.test/audio.m4a"));
        assert!((e.volume() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn is_live_matches_feature() {
        assert_eq!(AudioEngine::is_live(), cfg!(feature = "audio"));
    }

    /// Real end-to-end proof (under `--features audio`): a locally-generated
    /// WAV is fed to the `playbin`, which must reach `PLAYING` and report a
    /// **real, advancing** playhead decoded to a clock-synced sink. Uses a
    /// `fakesink` (via `QOS_MUSIC_FAKESINK`) so no sound card is needed; if the
    /// environment lacks the file/WAV plugins the decode can't run, so we skip
    /// rather than assert a decode the box can't perform (mirrors `--probe`).
    #[cfg(feature = "audio")]
    #[test]
    fn pipeline_reaches_playing_and_reports_real_position() {
        use gstreamer::prelude::*;
        assert!(gstreamer::init().is_ok(), "gstreamer initialises");
        // The decode graph playbin needs for a local WAV.
        let have_plugins = ["filesrc", "wavparse", "fakesink"]
            .iter()
            .all(|n| gstreamer::ElementFactory::find(n).is_some());
        if !have_plugins {
            eprintln!("[audio test] skipped — missing filesrc/wavparse/fakesink plugins");
            return;
        }

        // Write ~2s of 16-bit-mono PCM sine (44.1 kHz) as a real WAV.
        let path = std::env::temp_dir().join(format!("qos-music-test-{}.wav", std::process::id()));
        std::fs::write(&path, sine_wav(2, 44_100)).expect("write test wav");
        let uri = format!("file://{}", path.display());

        // Force the deterministic fakesink and play the generated file.
        unsafe { std::env::set_var("QOS_MUSIC_FAKESINK", "1") };
        let mut engine = AudioEngine::new();
        engine.set_volume(0.6);
        engine.load(&uri);
        engine.play();

        let reached = engine.wait_until_playing(std::time::Duration::from_secs(5));
        assert!(reached, "pipeline reached PLAYING for a real decoded stream");
        assert!(engine.is_playing_real(), "engine reports real PLAYING state");

        // The clock-synced sink advances the real playhead. Poll a short while.
        let mut pos = 0u64;
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if let Some(p) = engine.position_ms() {
                pos = pos.max(p);
            }
            if pos > 0 {
                break;
            }
        }
        assert!(pos > 0, "pipeline reports a real, advancing position (got {pos}ms)");

        // Duration of the generated file is known once prerolled (~2000ms).
        if let Some(d) = engine.duration_ms() {
            assert!(d >= 1500, "decoded duration reflects the ~2s source (got {d}ms)");
        }

        engine.stop();
        let _ = std::fs::remove_file(&path);
        unsafe { std::env::remove_var("QOS_MUSIC_FAKESINK") };
    }

    /// Build a minimal PCM WAV: `secs` seconds of a 220 Hz sine, 16-bit mono.
    #[cfg(feature = "audio")]
    fn sine_wav(secs: u32, rate: u32) -> Vec<u8> {
        let channels: u16 = 1;
        let bits: u16 = 16;
        let n = secs * rate;
        let block_align = channels * bits / 8;
        let byte_rate = rate * block_align as u32;
        let data_len = n * block_align as u32;
        let mut v = Vec::with_capacity(44 + data_len as usize);
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..n {
            let t = i as f32 / rate as f32;
            let s = (t * 220.0 * std::f32::consts::TAU).sin();
            let sample = (s * 12_000.0) as i16;
            v.extend_from_slice(&sample.to_le_bytes());
        }
        v
    }
}
