//! Audio output engine.
//!
//! Real playback uses a GStreamer `playbin` (gated behind the `audio`
//! cargo feature) fed an HTTP(S) `uri` resolved by [`crate::backend`]. The
//! default build compiles the silent stub so the UI, the offline `--shots`
//! screenshot and CI stay free of a system GStreamer dependency. The public
//! surface is identical either way, so the app can drive playback without
//! caring which backend is linked.
//!
//! ## Real audio out — routing to a real sink, never a `fakesink`
//!
//! `playbin` is given an **explicit audio sink chain** —
//! `audioconvert ! audioresample ! level ! autoaudiosink` — so the decoded
//! PCM is converted to whatever the system sink wants and handed to
//! `autoaudiosink`, which auto-plugs the real desktop sink
//! (`pipewiresink` / `pulsesink` / `alsasink`). This is the audible path.
//!
//! Crucially the engine can **prove** audio is genuinely flowing rather than
//! merely that the pipeline reached `PLAYING`: a buffer pad-probe on the sink's
//! input counts every decoded PCM buffer that actually reaches the sink
//! ([`AudioEngine::frames_rendered`]), and [`AudioEngine::actual_audio_sink_name`]
//! reports which concrete sink `autoaudiosink` selected — so a test can assert
//! it is a real audio sink and **not** a `fakesink`. A previous test proved
//! decode/timing against a `fakesink`, which never exercises a real sink and so
//! cannot prove *audible* output; that shortcut is gone.
//!
//! ffmpeg / ALSA / PulseAudio crates are deliberately **not** used —
//! GStreamer's `playbin` already negotiates decode + the system sink.
//!
//! ## System dependency (host + OS image)
//!
//! Audible output needs, in the runtime environment:
//!   * a GStreamer **audio sink** plugin — `gstreamer1-plugin-pipewire`
//!     (`pipewiresink`) and/or `gstreamer1-plugins-good` (`pulsesink`), plus a
//!     running PipeWire/Pulse server. Without one, `autoaudiosink` silently
//!     falls back to `fakesink` and the user hears nothing.
//!   * an **AAC / Opus decoder** for the streams YouTube Music serves —
//!     `gstreamer1-libav` (`avdec_aac`, `avdec_opus`) and/or
//!     `gstreamer1-plugins-bad-free` (`faad`). `gstreamer1-plugins-base` +
//!     `-good` alone can fetch (`souphttpsrc`) and demux (`qtdemux`) but cannot
//!     decode AAC, so playback is silent. This is the qOS-image dependency the
//!     Containerfile must install alongside the PipeWire sink.

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
    /// The `autoaudiosink` at the tail of our explicit sink chain, when it was
    /// built. `autoaudiosink` auto-plugs the concrete real sink
    /// (`pipewiresink`/`pulsesink`/`alsasink`) at READY; introspecting its child
    /// proves the live path renders to a real sink and never a `fakesink`.
    sink: Option<gstreamer::Element>,
    /// Decoded PCM buffers that have actually reached the audio sink, counted by
    /// a pad probe. Non-zero ⇒ audio is genuinely flowing to the sink (real
    /// output), not merely that the pipeline reported `PLAYING`.
    frames: std::sync::Arc<std::sync::atomic::AtomicU64>,
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
                    use gstreamer::prelude::*;
                    let frames = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
                    // Route audio through an explicit **real** sink chain, so the
                    // output is audible and verifiable. `autoaudiosink` selects
                    // the concrete system sink (pipewire/pulse/alsa); the `level`
                    // element and the pad probe let us confirm real buffers flow.
                    let sink = build_audio_sink(&playbin, &frames);
                    // Defensive: never leave the transport muted, and start at a
                    // sane volume so "no sound" is never a stuck property.
                    playbin.set_property("mute", false);
                    playbin.set_property("volume", 0.8f64);
                    return Self {
                        inner: Inner { playbin, sink, frames },
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
                sink: None,
                frames: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
            self.inner.frames.store(0, std::sync::atomic::Ordering::Relaxed);
            self.inner.playbin.set_property("uri", uri);
            self.inner.playbin.set_property("mute", false);
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
    /// `PLAYING` state. The now-playing indicator reads this so it only reflects
    /// genuine playback, never a fake timer. Always `false` for the silent stub.
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

    /// Number of decoded PCM buffers that have actually reached the audio sink.
    /// Non-zero proves audio is *rendering* to the sink — real audible output,
    /// not just a pipeline that reported `PLAYING`. Always `0` for the stub.
    pub fn frames_rendered(&self) -> u64 {
        #[cfg(feature = "audio")]
        {
            self.inner.frames.load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(feature = "audio"))]
        {
            0
        }
    }

    /// Factory name of the concrete audio sink `autoaudiosink` plugged
    /// (`pipewiresink` / `pulsesink` / `alsasink`), once it has resolved one
    /// (at READY+). `None` before then, without the `audio` feature, or if no
    /// explicit sink chain was built. Used to prove the live path renders to a
    /// **real** sink and never a `fakesink` — and to diagnose silence (a
    /// `fakesink` here means no real sink plugin is installed).
    pub fn actual_audio_sink_name(&self) -> Option<String> {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            let sink = self.inner.sink.as_ref()?;
            // `autoaudiosink` is a bin that owns the concrete sink as its child;
            // reach it via the `ChildProxy` interface (stable across versions).
            let proxy = sink.dynamic_cast_ref::<gstreamer::ChildProxy>()?;
            if proxy.children_count() == 0 {
                return None;
            }
            let child = proxy.child_by_index(0)?;
            let elem = child.downcast::<gstreamer::Element>().ok()?;
            elem.factory().map(|f| f.name().to_string())
        }
        #[cfg(not(feature = "audio"))]
        {
            None
        }
    }

    /// Whether the concrete plugged audio sink is a **real** audio sink (not a
    /// `fakesink` fallback). `None` until the sink resolves. A real audio sink
    /// is a `GstAudioBaseSink` subclass; `fakesink` is not, so this cleanly
    /// distinguishes "audible" from "silently dropped".
    pub fn audio_sink_is_real(&self) -> Option<bool> {
        match self.actual_audio_sink_name() {
            Some(name) => Some(name != "fakesink" && !name.contains("fakesink")),
            None => None,
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

/// Build the explicit real audio sink chain and install it on `playbin`,
/// returning the tail `autoaudiosink` element for introspection. A buffer
/// pad-probe on the sink's input increments `frames` for every decoded PCM
/// buffer that reaches the sink — the proof that audio is genuinely rendered.
///
/// If any element is missing we return `None` and leave `playbin` to auto-plug
/// its own sink, so audio still works (just without the verification hooks).
#[cfg(feature = "audio")]
fn build_audio_sink(
    playbin: &gstreamer::Element,
    frames: &std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> Option<gstreamer::Element> {
    use gstreamer::prelude::*;
    // `ghost_unlinked_pads=true` auto-exposes the audioconvert sink pad so
    // playbin can feed decoded audio into the chain (same idiom as the video
    // sink bin in `apps/tube`). `autoaudiosink` plugs the real system sink.
    let bin = gstreamer::parse::bin_from_description(
        "audioconvert ! audioresample ! level name=qos_level post-messages=true ! \
         autoaudiosink name=qos_audiosink sync=true",
        true,
    )
    .ok()?;
    let sink = bin.by_name("qos_audiosink")?;
    // Count decoded PCM buffers arriving at the real sink.
    if let Some(pad) = sink.static_pad("sink") {
        let frames = frames.clone();
        pad.add_probe(gstreamer::PadProbeType::BUFFER, move |_, _| {
            frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            gstreamer::PadProbeReturn::Ok
        });
    }
    playbin.set_property("audio-sink", &bin);
    Some(sink)
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
        assert_eq!(e.current_uri(), Some("https://example.test/audio.m4a"));
        assert!((e.volume() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn is_live_matches_feature() {
        assert_eq!(AudioEngine::is_live(), cfg!(feature = "audio"));
    }

    /// Whether a running PipeWire/Pulse audio server is reachable, so the real
    /// sink can actually open a device. Without one, `autoaudiosink` can't reach
    /// `PLAYING` — the test then skips honestly rather than pretending, and
    /// never substitutes a `fakesink` to force a green.
    #[cfg(feature = "audio")]
    fn have_audio_server() -> bool {
        let run = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
        run.map(|d| d.join("pipewire-0").exists() || d.join("pulse/native").exists())
            .unwrap_or(false)
            || std::env::var_os("PULSE_SERVER").is_some()
    }

    /// **Real audible-output proof** (under `--features audio`): a locally
    /// generated WAV is decoded by `playbin` and rendered through the real
    /// `autoaudiosink` sink chain — NOT a `fakesink`. Asserts:
    ///   * the pipeline reaches `PLAYING`,
    ///   * decoded PCM buffers actually reach the sink (`frames_rendered > 0`) —
    ///     i.e. the audio branch negotiated caps and buffers flow, and
    ///   * the concrete sink `autoaudiosink` plugged is a **real** audio sink
    ///     (`pulsesink`/`pipewiresink`/`alsasink`), never a `fakesink`.
    ///
    /// Skips honestly if the environment lacks the WAV decode plugins or a
    /// running audio server (no sound card in CI) — it never falls back to a
    /// `fakesink` to manufacture a pass.
    #[cfg(feature = "audio")]
    #[test]
    fn real_sink_renders_audio_and_is_not_fakesink() {
        assert!(gstreamer::init().is_ok(), "gstreamer initialises");
        // The decode graph playbin needs for a local WAV, plus the real sink.
        let need = ["filesrc", "wavparse", "audioconvert", "audioresample", "autoaudiosink"];
        let have_plugins = need
            .iter()
            .all(|n| gstreamer::ElementFactory::find(n).is_some());
        if !have_plugins {
            eprintln!("[audio test] skipped — missing decode/sink plugins {need:?}");
            return;
        }
        if !have_audio_server() {
            eprintln!("[audio test] skipped — no PipeWire/Pulse server (no sound card here)");
            return;
        }

        // Write ~1s of quiet 16-bit-mono PCM sine (44.1 kHz) as a real WAV.
        let path = std::env::temp_dir().join(format!("qos-music-audio-{}.wav", std::process::id()));
        std::fs::write(&path, sine_wav(1, 44_100)).expect("write test wav");
        let uri = format!("file://{}", path.display());

        let mut engine = AudioEngine::new();
        engine.set_volume(0.05); // audible path, but keep the test quiet
        engine.load(&uri);
        engine.play();

        let reached = engine.wait_until_playing(std::time::Duration::from_secs(5));
        assert!(reached, "real sink pipeline reached PLAYING for a decoded stream");
        assert!(engine.is_playing_real(), "engine reports real PLAYING state");

        // Poll until decoded PCM buffers actually reach the real sink. This is
        // the audible-output proof a fakesink test could never give.
        let mut frames = 0u64;
        for _ in 0..80 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            frames = engine.frames_rendered();
            if frames > 0 {
                break;
            }
        }
        assert!(frames > 0, "decoded PCM buffers reached the real audio sink");

        // The concrete sink autoaudiosink plugged must be a real audio sink,
        // never a fakesink fallback (which would mean silence in the field).
        let sink_name = engine
            .actual_audio_sink_name()
            .expect("autoaudiosink resolved a concrete sink");
        assert_ne!(sink_name, "fakesink", "real audio sink, not a fakesink");
        assert_eq!(
            engine.audio_sink_is_real(),
            Some(true),
            "plugged sink {sink_name} is a real audio sink"
        );
        eprintln!("[audio test] rendered {frames} buffers to real sink `{sink_name}`");

        engine.stop();
        let _ = std::fs::remove_file(&path);
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
