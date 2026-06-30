//! Audio output engine.
//!
//! Real playback uses a GStreamer `playbin` (gated behind the `audio`
//! cargo feature) fed an HTTP(S) `uri` resolved by [`crate::source`]. The
//! default build compiles the silent stub so the UI, the offline `--shots`
//! screenshot and CI stay free of a system GStreamer dependency. The public
//! surface is identical either way, so the app can drive playback without
//! caring which backend is linked.
//!
//! ffmpeg / ALSA / PulseAudio crates are deliberately **not** used —
//! GStreamer's `playbin` already negotiates decode + the system sink.

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
            let _ = self.inner.playbin.set_state(gstreamer::State::Null);
            self.inner.playbin.set_property("uri", uri);
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
}
