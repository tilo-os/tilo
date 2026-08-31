//! Audio output engine.
//!
//! Real playback uses a `GStreamer` `playbin` (gated behind the `audio` cargo
//! feature). The default build compiles a silent stub with the same public
//! surface, so the UI, the offline `--shots` screenshot and CI stay free of a
//! system `GStreamer` dependency.
//!
//! ## Real audio out — an explicit sink chain, never a `fakesink`
//!
//! `playbin` is given an explicit audio sink chain —
//! `audioconvert ! audioresample ! autoaudiosink` — so decoded PCM is
//! converted to whatever the system sink wants and handed to `autoaudiosink`,
//! which auto-plugs the real desktop sink (`pipewiresink` / `pulsesink` /
//! `alsasink`). A buffer pad-probe on the sink's input counts every decoded
//! PCM buffer that actually reaches it ([`AudioEngine::frames_rendered`]), and
//! [`AudioEngine::actual_audio_sink_name`] reports which concrete sink
//! `autoaudiosink` selected, so a test can assert real audio is flowing to a
//! real sink and not merely that the pipeline reached `PLAYING`.
//!
//! ## Fetching googlevideo streams
//!
//! A resolved `YouTube` stream URL can't be handed straight to playbin's
//! `souphttpsrc`: to enable seeking it issues an open-ended `Range: bytes=0-`
//! request, and googlevideo answers that with `403 Forbidden` — it only serves
//! **bounded** byte ranges. So the engine stages the stream to a temp file
//! with `reqwest` using bounded `Range` requests instead, then plays that
//! local file through the ordinary playbin path — see
//! [`spawn_stream_download`] and [`AudioEngine::pump`]. A `file://` uri (a
//! cached track) plays directly. The engine also services the `GStreamer` bus
//! so a genuinely failed pipeline is never reported as still playing.
//!
//! ffmpeg / ALSA / `PulseAudio` crates are deliberately not used — `playbin`
//! already negotiates decode + the system sink.
//!
//! ## System dependency (host + OS image)
//!
//! Audible output needs, in the runtime environment:
//! - a `GStreamer` **audio sink** plugin — `gstreamer1-plugin-pipewire`
//!   (`pipewiresink`) and/or `gstreamer1-plugins-good` (`pulsesink`), plus a
//!   running PipeWire/Pulse server. Without one, `autoaudiosink` silently
//!   falls back to `fakesink` and the user hears nothing.
//! - an **AAC / Opus decoder** for the streams `YouTube` Music serves —
//!   `gstreamer1-libav` (`avdec_aac`, `avdec_opus`) and/or
//!   `gstreamer1-plugins-bad-free` (`faad`). `gstreamer1-plugins-base` +
//!   `-good` can demux (`qtdemux`) the staged file but cannot decode AAC, so
//!   playback is silent without one. (The stream itself is fetched by
//!   `reqwest`, so no `GStreamer` HTTP-source plugin is needed.)

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
    /// Set when the pipeline posts a fatal error on its bus (a forbidden fetch,
    /// an undecodable stream). The transport reads it so it never *pretends* to
    /// play a dead pipeline (see [`AudioEngine::is_playing_real`]).
    errored: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The human-readable text of the last fatal bus error, so the transport can
    /// surface a real message ("Can't stream: …") instead of silently freezing.
    error_msg: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Set when the pipeline posts end-of-stream. A `YouTube` "free" (no-PoToken)
    /// direct URL is SABR-gated to its first ~1-minute segment, so the staged
    /// file ends early and the pipeline EOSes before the track's metadata
    /// duration; the transport reads this to advance to the next track instead
    /// of freezing at the truncation point.
    eos: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// An in-flight HTTP stream being downloaded (bounded reqwest ranges) into a
    /// temp file before playback. `None` for a local (`file://`) uri, which is
    /// handed straight to playbin. Only present in a build that can fetch
    /// (`live`); see [`AudioEngine::begin_stream_download`].
    #[cfg(feature = "live")]
    pending: Option<Pending>,
}

/// A live HTTP stream being staged to a temp file so it can be played locally.
///
/// googlevideo 403s playbin's `souphttpsrc` (its open-ended `Range: bytes=0-`
/// request is refused; `YouTube` only serves **bounded** ranges from byte 0), so
/// the media is fetched with `reqwest` — which *does* get bounded ranges — into
/// this temp file, then played through the ordinary (proven) playbin file path.
#[cfg(all(feature = "audio", feature = "live"))]
struct Pending {
    /// Temp file the stream is downloaded into.
    path: std::path::PathBuf,
    /// Set by the download thread after the first staged chunk lands — playback
    /// may begin (progressively) from whatever is on disk so far.
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set by the download thread once it has fetched the *entire* file (or given
    /// up). Distinct from `ready` (first-chunk): a seek to a late position is only
    /// safe once the bytes for it have landed, so the full-track proof waits on
    /// this before scrubbing near the end.
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Whether the transport wants to be playing once the file is ready.
    want_play: bool,
    /// Whether playback has already been switched onto the temp file.
    started: bool,
}

#[cfg(not(feature = "audio"))]
struct Inner {
    /// Stub bookkeeping: whether the (silent) transport is "playing".
    playing: bool,
}

impl AudioEngine {
    /// Build an engine. Infallible for the stub build. With `--features
    /// audio`, `playbin` is a core `GStreamer` element (`gstreamer1-plugins-base`)
    /// that this feature depends on, so a failure to construct it means the
    /// deployment is missing that runtime dependency rather than a condition
    /// callers can route around — this panics with a diagnostic rather than
    /// silently degrading to a stub that would claim to play and never sound.
    ///
    /// # Panics
    ///
    /// With the `audio` feature: if `playbin` (a core `GStreamer` element from
    /// `gstreamer1-plugins-base`) fails to construct, meaning that runtime
    /// dependency is missing from the deployment.
    #[allow(
        clippy::expect_used,
        reason = "a missing playbin means the audio feature's runtime dependency isn't installed; panicking with a diagnostic is the intended behaviour, not a bug to route around (see doc comment above)"
    )]
    pub fn new() -> Self {
        #[cfg(feature = "audio")]
        let inner = {
            use gstreamer::prelude::*;
            if let Err(e) = gstreamer::init() {
                tracing::error!(target: "music::audio", error = %e, "gstreamer::init failed — the `audio` feature needs a working GStreamer runtime");
            }
            let playbin = gstreamer::ElementFactory::make("playbin").build().expect(
                "playbin: requires gstreamer1-plugins-base, the audio feature's runtime dependency",
            );
            let frames = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            // Route audio through an explicit **real** sink chain, so the
            // output is audible and verifiable. `autoaudiosink` selects
            // the concrete system sink (pipewire/pulse/alsa); the pad
            // probe lets us confirm real buffers flow.
            let sink = build_audio_sink(&playbin, &frames);
            let errored = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            // Defensive: never leave the transport muted, and start at a
            // sane volume so "no sound" is never a stuck property.
            playbin.set_property("mute", false);
            playbin.set_property("volume", 0.8f64);
            Inner {
                playbin,
                sink,
                frames,
                errored,
                error_msg: std::sync::Arc::new(std::sync::Mutex::new(None)),
                eos: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                #[cfg(feature = "live")]
                pending: None,
            }
        };
        #[cfg(not(feature = "audio"))]
        let inner = Inner { playing: false };
        Self {
            inner,
            current_uri: None,
            volume: 0.8,
        }
    }

    /// Point the engine at a new stream URI and start from zero.
    pub fn load(&mut self, uri: &str) {
        self.load_with_fallback(uri, None);
    }

    /// Like [`Self::load`], but with a secondary source the staging download
    /// falls back to when `uri` yields no bytes (a flaky Invidious proxy → the
    /// direct googlevideo URL). `fallback` is ignored for a `file://` uri and in
    /// non-`live` builds.
    pub fn load_with_fallback(&mut self, uri: &str, fallback: Option<&str>) {
        self.current_uri = Some(uri.to_string());
        let _ = fallback;
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            // Fully reset the pipeline before re-pointing it, so a fast skip
            // never leaves the previous stream decoding underneath the new one.
            let _ = self.inner.playbin.set_state(gstreamer::State::Null);
            self.inner
                .frames
                .store(0, std::sync::atomic::Ordering::Relaxed);
            self.inner
                .errored
                .store(false, std::sync::atomic::Ordering::Relaxed);
            if let Ok(mut m) = self.inner.error_msg.lock() {
                *m = None;
            }
            self.inner
                .eos
                .store(false, std::sync::atomic::Ordering::Relaxed);
            tracing::info!(target: "music::audio", %uri, "load: reset pipeline for new stream");

            // An http(s) stream can't be handed to playbin's `souphttpsrc`:
            // googlevideo 403s its open-ended range request. Stage it to a temp
            // file via bounded reqwest ranges, then play that locally (below,
            // once the download is ready). A `file://` uri plays directly.
            #[cfg(feature = "live")]
            if uri.starts_with("http") {
                self.begin_stream_download(uri, fallback.map(str::to_string));
                return;
            }
            #[cfg(feature = "live")]
            self.clear_pending();

            self.inner.playbin.set_property("uri", uri);
            self.inner.playbin.set_property("mute", false);
            self.inner
                .playbin
                .set_property("volume", f64::from(self.volume));
        }
    }

    pub fn play(&mut self) {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            // A stream still downloading to its temp file starts when the file
            // is ready (in `pump`); record the intent and don't touch playbin yet.
            #[cfg(feature = "live")]
            if let Some(p) = &mut self.inner.pending {
                p.want_play = true;
                if !p.started {
                    return;
                }
            }
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
            #[cfg(feature = "live")]
            if let Some(p) = &mut self.inner.pending {
                p.want_play = false;
            }
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
            self.inner
                .eos
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.inner
                .errored
                .store(false, std::sync::atomic::Ordering::Relaxed);
            #[cfg(feature = "live")]
            self.clear_pending();
        }
        #[cfg(not(feature = "audio"))]
        {
            self.inner.playing = false;
        }
    }

    /// Seek to an absolute position in milliseconds.
    ///
    /// The target is clamped to the pipeline's *real* seekable duration (leaving
    /// a small guard before the end) so a seek can never land past EOS. That is
    /// the bug behind "can't seek while playing": the app computes a target from
    /// the track's `YouTube` metadata duration, but the media the pipeline holds
    /// may be shorter (a truncated stream) — seeking to metadata-50% then ran
    /// off the end of the file, the pipeline hit EOS, and the transport snapped
    /// to the end / advanced. With a fully-rangeable source and this clamp, a
    /// seek repositions the pipeline instead of ending the track.
    pub fn seek_ms(&mut self, ms: u64) {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            let target = match self.duration_ms() {
                // Never seek to (or past) the very end — keep a 250 ms guard so a
                // scrub to ~100% resumes just before the end rather than EOS-ing.
                Some(dur) if dur > 0 => ms.min(dur.saturating_sub(250)),
                _ => ms,
            };
            let _ = self.inner.playbin.seek_simple(
                gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::KEY_UNIT,
                gstreamer::ClockTime::from_mseconds(target),
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
            self.inner
                .playbin
                .set_property("volume", f64::from(self.volume));
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn current_uri(&self) -> Option<&str> {
        self.current_uri.as_deref()
    }

    /// Query the pipeline's current playhead in milliseconds, when the live
    /// `GStreamer` backend is running and a position is available. The UI uses
    /// this to drive a *real* (not simulated) playhead while audio plays;
    /// `None` falls back to the local simulated playhead.
    pub fn position_ms(&self) -> Option<u64> {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            self.inner
                .playbin
                .query_position::<gstreamer::ClockTime>()
                .map(gstreamer::ClockTime::mseconds)
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
            self.inner
                .playbin
                .query_duration::<gstreamer::ClockTime>()
                .map(gstreamer::ClockTime::mseconds)
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
            // A pipeline that errored (a forbidden fetch, an undecodable stream)
            // can linger with `pending == Playing`; never report that as playing,
            // or the transport fakes progress over a stream that makes no sound.
            if self
                .inner
                .errored
                .load(std::sync::atomic::Ordering::Relaxed)
            {
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

    /// Service the `GStreamer` bus (non-blocking): drain pending messages so they
    /// don't accumulate, and latch a fatal error so the transport stops
    /// pretending to play. Called once per frame from the app's tick. Also
    /// returns whether an error is currently latched, for diagnostics.
    pub fn pump(&mut self) -> bool {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            // Once a staged stream's temp file is ready, switch playback onto it.
            #[cfg(feature = "live")]
            {
                let staged = self.inner.pending.as_ref().and_then(|p| {
                    (!p.started && p.ready.load(std::sync::atomic::Ordering::Relaxed))
                        .then(|| (p.path.clone(), p.want_play))
                });
                if let Some((path, want)) = staged {
                    let has_bytes = std::fs::metadata(&path).is_ok_and(|m| m.len() > 0);
                    if has_bytes {
                        let uri = format!("file://{}", path.display());
                        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                        tracing::info!(target: "music::audio", %uri, bytes, want_play = want, "staged file ready — switching playbin to local file and rolling");
                        let _ = self.inner.playbin.set_state(gstreamer::State::Null);
                        self.inner.playbin.set_property("uri", &uri);
                        self.inner.playbin.set_property("mute", false);
                        self.inner
                            .playbin
                            .set_property("volume", f64::from(self.volume));
                        if want {
                            let _ = self.inner.playbin.set_state(gstreamer::State::Playing);
                        }
                    } else {
                        // The fetch was forbidden before any bytes landed: honest
                        // silence, never a transport that pretends to play.
                        tracing::warn!(target: "music::audio", "staged file has no bytes — no source served media; reporting error, not fake playback");
                        self.inner
                            .errored
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        if let Ok(mut m) = self.inner.error_msg.lock()
                            && m.is_none()
                        {
                            *m = Some("no audio source served bytes".to_string());
                        }
                    }
                    if let Some(p) = &mut self.inner.pending {
                        p.started = true;
                    }
                }
            }
            if let Some(bus) = self.inner.playbin.bus() {
                while let Some(msg) = bus.pop() {
                    match msg.view() {
                        gstreamer::MessageView::Error(err) => {
                            tracing::error!(target: "music::audio", error = %err.error(), debug = ?err.debug(), "pipeline bus ERROR");
                            self.inner
                                .errored
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                            if let Ok(mut m) = self.inner.error_msg.lock() {
                                *m = Some(err.error().to_string());
                            }
                        }
                        gstreamer::MessageView::Eos(_) => {
                            tracing::info!(target: "music::audio", "pipeline bus EOS");
                            self.inner
                                .eos
                                .store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        gstreamer::MessageView::StateChanged(sc) => {
                            // Only the playbin's own transitions are interesting
                            // (match by element name to avoid a type mismatch on
                            // the `Object`/`Element` comparison).
                            let pb_name = self.inner.playbin.name();
                            if sc.src().is_some_and(|s| s.name() == pb_name) {
                                tracing::debug!(target: "music::audio", from = ?sc.old(), to = ?sc.current(), pending = ?sc.pending(), "playbin state changed");
                            }
                        }
                        _ => {}
                    }
                }
            }
            self.inner
                .errored
                .load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(feature = "audio"))]
        {
            false
        }
    }

    /// Take (and clear) the last fatal pipeline error text, if any. The
    /// transport calls this once it has seen [`Self::pump`] report an error, so
    /// it can surface a real message to the user and stop pretending to play a
    /// dead pipeline. Always `None` for the silent stub.
    pub fn take_error(&self) -> Option<String> {
        #[cfg(feature = "audio")]
        {
            self.inner.error_msg.lock().ok().and_then(|mut m| m.take())
        }
        #[cfg(not(feature = "audio"))]
        {
            None
        }
    }

    /// Whether the pipeline has reached end-of-stream. A free (no-PoToken)
    /// `YouTube` direct URL is SABR-gated to its first ~1-minute segment, so the
    /// staged file ends before the track's metadata duration and the pipeline
    /// EOSes early; the transport reads this to advance to the next track rather
    /// than freeze at the truncation point. Always `false` for the silent stub.
    pub fn reached_eos(&self) -> bool {
        #[cfg(feature = "audio")]
        {
            self.inner.eos.load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(feature = "audio"))]
        {
            false
        }
    }

    /// Whether the staging download (if any) has fetched the **entire** file, so
    /// a seek to a late position is backed by bytes on disk. `true` when there is
    /// no in-flight staging (a local `file://` uri, or nothing loaded). Used by
    /// the full-track proof to wait before scrubbing past the 1-minute mark.
    pub fn staging_complete(&self) -> bool {
        #[cfg(all(feature = "audio", feature = "live"))]
        {
            match &self.inner.pending {
                Some(p) => p.done.load(std::sync::atomic::Ordering::Relaxed),
                None => true,
            }
        }
        #[cfg(not(all(feature = "audio", feature = "live")))]
        {
            true
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
        self.actual_audio_sink_name()
            .map(|name| name != "fakesink" && !name.contains("fakesink"))
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
            if res.is_err() {
                // A hard state-change error (missing plugin, unreadable uri).
                return false;
            }
        }
    }

    /// Whether real audio output is wired up in this build.
    pub fn is_live() -> bool {
        cfg!(feature = "audio")
    }

    /// Begin staging an http(s) stream to a temp file via bounded reqwest ranges.
    /// Non-blocking: the fetch runs on its own thread; [`Self::pump`] switches
    /// playback onto the file once it is ready.
    #[cfg(all(feature = "audio", feature = "live"))]
    fn begin_stream_download(&mut self, uri: &str, fallback: Option<String>) {
        self.clear_pending();
        let path = std::env::temp_dir().join(format!(
            "qos-music-stream-{}-{}.bin",
            std::process::id(),
            next_stream_id()
        ));
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        spawn_stream_download(
            uri.to_string(),
            fallback,
            path.clone(),
            ready.clone(),
            done.clone(),
        );
        self.inner.pending = Some(Pending {
            path,
            ready,
            done,
            want_play: false,
            started: false,
        });
    }

    /// Drop any staged stream, removing its temp file.
    #[cfg(all(feature = "audio", feature = "live"))]
    fn clear_pending(&mut self) {
        if let Some(p) = self.inner.pending.take() {
            let _ = std::fs::remove_file(&p.path);
        }
    }
}

/// The HTTP `User-Agent`s of the player clients rustypipe resolves streams
/// through (its default order with no `BotGuard` is `[Ios, Tv]`). We present the
/// client UA matching the URL's `c=` param on the media fetch to mirror the
/// client that signed it. Values mirror rustypipe's own client UAs (crate 0.11).
#[cfg(all(feature = "audio", feature = "live"))]
const IOS_UA: &str =
    "com.google.ios.youtube/20.03.02 (iPhone16,2; U; CPU iOS 18_2_1 like Mac OS X)";
#[cfg(all(feature = "audio", feature = "live"))]
const TV_UA: &str = "Mozilla/5.0 (SMART-TV; Linux; Tizen 5.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/5.0 NativeTVAds Safari/538.1";

/// Pick the `User-Agent` for fetching a media URL.
///
/// A signed **googlevideo** URL is presented with the client UA matching its
/// `c=` query param (`c=IOS`, `c=TV`, …), mirroring the client that signed it.
/// Any other source (notably an Invidious `local` proxy) gets a plain browser
/// UA — those proxies expect a normal browser, not the iOS `YouTube` app.
#[cfg(all(feature = "audio", feature = "live"))]
fn ua_for_uri(uri: &str) -> &'static str {
    if uri.contains("googlevideo.com") {
        let client = uri.split(['?', '&']).find_map(|kv| kv.strip_prefix("c="));
        return match client {
            Some(c) if c.starts_with("TV") => TV_UA,
            _ => IOS_UA,
        };
    }
    crate::backend::proxy::BROWSER_UA
}

/// Parse the `clen=` (content-length) query param off a googlevideo url, so the
/// download knows when it has fetched the whole stream.
#[cfg(all(feature = "audio", feature = "live"))]
fn parse_clen(uri: &str) -> Option<u64> {
    uri.split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("clen="))
        .and_then(|v| v.parse().ok())
}

/// A process-unique id so concurrent/successive streams get distinct temp files.
#[cfg(all(feature = "audio", feature = "live"))]
fn next_stream_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Download an http(s) media stream into `path` on a background thread, using
/// **bounded** `Range` requests (`bytes=off-off+chunk`) — the only form
/// googlevideo serves (it 403s the open-ended range playbin's `souphttpsrc`
/// sends). Stops at end-of-stream, a short read, or the first refused range
/// (a PoToken-gated stream then yields as much as the server served). Sets
/// `ready` when finished so the engine can begin playback from the file.
#[cfg(all(feature = "audio", feature = "live"))]
fn spawn_stream_download(
    uri: String,
    fallback: Option<String>,
    path: std::path::PathBuf,
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let _ = std::thread::Builder::new()
        .name("qos-music-stream-dl".into())
        .spawn(move || {
            let finish = || {
                ready.store(true, Relaxed);
                done.store(true, Relaxed);
            };
            let Ok(mut file) = std::fs::File::create(&path) else {
                return finish();
            };
            // Primary source (the rangeable proxy). If it yields no bytes at all
            // — a broken/rate-limited public proxy — fall back to the direct
            // googlevideo URL so playback still works (at least its first
            // segment). Both stream progressively: `ready` flips after the first
            // staged chunk so audio starts without waiting for the full download.
            tracing::info!(target: "music::audio", %uri, has_fallback = fallback.is_some(), "stream staging: begin (primary source)");
            let mut wrote = download_into(&uri, &mut file, &ready);
            if wrote == 0
                && let Some(fb) = fallback
            {
                tracing::info!(target: "music::audio", fallback = %fb, "stream staging: primary yielded no media, trying fallback (direct)");
                wrote = download_into(&fb, &mut file, &ready);
            }
            tracing::info!(target: "music::audio", bytes = wrote, "stream staging: finished");
            let _ = std::io::Write::flush(&mut file);
            finish();
        });
}

/// Download `uri` into `file` using **bounded** `Range` requests (the only form
/// googlevideo serves; a proxy serves them over the whole file), flipping
/// `ready` after the first staged chunk so playback can start progressively.
/// Returns the number of bytes written.
#[cfg(all(feature = "audio", feature = "live"))]
fn download_into(
    uri: &str,
    file: &mut std::fs::File,
    ready: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> u64 {
    use std::sync::atomic::Ordering::Relaxed;
    const CHUNK: u64 = 1_048_576;
    let Ok(client) = reqwest::blocking::Client::builder()
        .user_agent(ua_for_uri(uri))
        .timeout(std::time::Duration::from_secs(30))
        .build()
    else {
        return 0;
    };
    let clen = parse_clen(uri);
    let mut off = 0u64;
    loop {
        if let Some(c) = clen
            && off >= c
        {
            break;
        }
        let end = match clen {
            Some(c) => (off + CHUNK - 1).min(c - 1),
            None => off + CHUNK - 1,
        };
        let want = end - off + 1;
        let resp = client
            .get(uri)
            .header(reqwest::header::RANGE, format!("bytes={off}-{end}"))
            .send();
        let Ok(resp) = resp else {
            tracing::debug!(target: "music::audio", %uri, off, "stream fetch: request failed");
            break;
        };
        if !resp.status().is_success() {
            tracing::debug!(target: "music::audio", %uri, off, status = %resp.status(), "stream fetch: non-success status");
            break;
        }
        // Guard against a proxy that answers 2xx with an HTML anti-bot / error
        // page instead of media: staging that into the pipeline makes GStreamer
        // error out and the transport freeze at 0:00. On the very first chunk we
        // check the Content-Type and sniff the body; if it isn't media we write
        // nothing and return 0 so the caller falls back to the direct URL.
        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if off == 0 && !ct.is_empty() && !crate::backend::proxy::content_type_is_media(&ct) {
            tracing::warn!(target: "music::audio", %uri, content_type = %ct, "stream fetch: non-media Content-Type — discarding, will try fallback");
            break;
        }
        let Ok(bytes) = resp.bytes() else { break };
        if bytes.is_empty() {
            break;
        }
        if off == 0 && crate::backend::proxy::body_sniffs_as_html(&bytes) {
            tracing::warn!(target: "music::audio", %uri, "stream fetch: body sniffs as HTML — discarding, will try fallback");
            break;
        }
        if std::io::Write::write_all(file, &bytes).is_err() {
            break;
        }
        let _ = std::io::Write::flush(file);
        let n = bytes.len() as u64;
        off += n;
        // Progressive start: the first staged chunk (the moov header + initial
        // samples) is enough to begin playback, so mark the file playable now and
        // keep fetching the rest into the SAME file. The whole track still lands
        // (so it's fully seekable — see the seek clamp), but audio starts in ~a
        // chunk instead of after the entire download. Playback (real-time, ~16
        // KB/s for 128 kbps) never outruns the network download, so the demuxer
        // doesn't hit EOF.
        ready.store(true, Relaxed);
        if n < want {
            break; // short read ⇒ end of stream
        }
    }
    off
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
        "audioconvert ! audioresample ! autoaudiosink name=qos_audiosink sync=true",
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
        #[cfg(feature = "live")]
        if let Some(p) = self.inner.pending.take() {
            let _ = std::fs::remove_file(&p.path);
        }
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
        run.is_some_and(|d| d.join("pipewire-0").exists() || d.join("pulse/native").exists())
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
        let need = [
            "filesrc",
            "wavparse",
            "audioconvert",
            "audioresample",
            "autoaudiosink",
        ];
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
        assert!(
            reached,
            "real sink pipeline reached PLAYING for a decoded stream"
        );
        assert!(
            engine.is_playing_real(),
            "engine reports real PLAYING state"
        );

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
        assert!(
            frames > 0,
            "decoded PCM buffers reached the real audio sink"
        );

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

    /// **Seek-while-playing proof** (`--features audio`): a real decoded WAV is
    /// playing, then a seek to ~50% must reposition the transport (position lands
    /// near 50%) and playback must *continue* — not snap to the end / EOS, which
    /// was the reported "can't seek while playing" bug. Uses a fully-seekable
    /// local file so this exercises the real `seek_simple` path.
    ///
    /// Skips honestly when the decode/sink plugins or an audio server are absent.
    #[cfg(feature = "audio")]
    #[test]
    fn seek_while_playing_repositions_and_keeps_playing() {
        assert!(gstreamer::init().is_ok(), "gstreamer initialises");
        let need = [
            "filesrc",
            "wavparse",
            "audioconvert",
            "audioresample",
            "autoaudiosink",
        ];
        if !need
            .iter()
            .all(|n| gstreamer::ElementFactory::find(n).is_some())
        {
            eprintln!("[seek test] skipped — missing decode/sink plugins {need:?}");
            return;
        }
        if !have_audio_server() {
            eprintln!("[seek test] skipped — no PipeWire/Pulse server");
            return;
        }

        // ~6 s of audio so a 50% seek lands at a clear ~3 s offset.
        let path = std::env::temp_dir().join(format!("qos-music-seek-{}.wav", std::process::id()));
        std::fs::write(&path, sine_wav(6, 44_100)).expect("write test wav");
        let uri = format!("file://{}", path.display());

        let mut engine = AudioEngine::new();
        engine.set_volume(0.05);
        engine.load(&uri);
        engine.play();
        assert!(
            engine.wait_until_playing(std::time::Duration::from_secs(5)),
            "pipeline reached PLAYING"
        );

        // Duration should be known now (the WAV header carries it); the seek
        // clamp reads it.
        let dur = {
            let mut d = None;
            for _ in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                d = engine.duration_ms();
                if d.is_some() {
                    break;
                }
            }
            d.expect("duration resolved for a local WAV")
        };
        assert!(
            dur >= 5_000,
            "≈6 s WAV reports a multi-second duration ({dur} ms)"
        );

        // Seek to ~50% while playing.
        let target = dur / 2;
        engine.seek_ms(target);

        // The position must settle near the target — NOT at the end — and the
        // pipeline must still be playing (never EOS'd by the seek).
        let mut pos = 0;
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            if let Some(p) = engine.position_ms() {
                pos = p;
                if p >= target.saturating_sub(700) && p <= target + 1_500 {
                    break;
                }
            }
        }
        assert!(
            pos >= target.saturating_sub(700) && pos < dur.saturating_sub(100),
            "seek landed near 50% ({pos} ms) and not at the end ({dur} ms)"
        );
        assert!(
            engine.is_playing_real(),
            "playback continued after the seek (no EOS)"
        );
        // Frames keep flowing after the seek — genuinely audible, still decoding.
        let before = engine.frames_rendered();
        let mut after = before;
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(25));
            after = engine.frames_rendered();
            if after > before {
                break;
            }
        }
        assert!(
            after > before,
            "decoded buffers keep reaching the sink after the seek"
        );

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
        let byte_rate = rate * u32::from(block_align);
        let data_len = n * u32::from(block_align);
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
