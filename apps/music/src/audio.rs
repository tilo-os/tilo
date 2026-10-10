//! Audio output engine.
//!
//! Real playback uses a `GStreamer` `playbin` (gated behind the `audio` cargo
//! feature, enabled by default). Builds without `audio` use a silent stub
//! with the same public surface; those builds can render screenshots without
//! a system `GStreamer` dependency.
//!
//! ## Real audio out — an explicit sink chain, never a `fakesink`
//!
//! `playbin` is given an explicit audio sink chain —
//! `audioconvert ! audioresample ! autoaudiosink` — so decoded PCM is
//! converted to whatever the system sink wants and handed to `autoaudiosink`,
//! which auto-plugs the real desktop sink (`pipewiresink` / `pulsesink` /
//! `alsasink` on Linux, WASAPI on Windows). A buffer pad-probe on the sink's input counts every decoded
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
//! `spawn_stream_download` and [`AudioEngine::pump`]. A `file://` uri (a
//! cached track) plays directly. The engine also services the `GStreamer` bus
//! so a genuinely failed pipeline is never reported as still playing.
//!
//! ffmpeg / ALSA / `PulseAudio` crates are deliberately not used — `playbin`
//! already negotiates decode + the system sink.
//!
//! ## System dependency (host + OS image)
//!
//! Audible output needs the host `GStreamer` runtime, including an audio sink
//! (WASAPI on Windows) and AAC/Opus decoders. On Fedora, install:
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

#[cfg(feature = "spotify-native")]
pub(crate) use tilo_spotify as spotify_audio;
#[cfg(feature = "audio")]
mod pipeline_controls;

/// A bounded initialization job. Cancellation drops its result receiver without
/// joining a potentially slow native initializer. A replacement waits for this
/// worker to finish, so repeated load/stop never creates parallel initializers.
struct Initialization<T> {
    result: Option<std::sync::mpsc::Receiver<Result<T, String>>>,
    #[cfg_attr(
        not(feature = "audio"),
        allow(
            dead_code,
            reason = "only the GStreamer backend initializes on a worker"
        )
    )]
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<T> Initialization<T> {
    /// A job that already finished on the caller's thread: a backend that
    /// needs no native initialization (the page's media element).
    #[cfg_attr(
        feature = "audio",
        allow(
            dead_code,
            reason = "the GStreamer build always initializes on a worker"
        )
    )]
    fn ready(result: Result<T, String>) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ = sender.send(result);
        Self {
            result: Some(receiver),
            finished: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    fn poll(&self) -> Option<Result<T, String>> {
        match self.result.as_ref()?.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(
                "audio initialization worker stopped without a result".into(),
            )),
        }
    }

    fn cancel(&mut self) {
        self.result = None;
    }

    #[cfg_attr(
        not(feature = "audio"),
        allow(
            dead_code,
            reason = "only the GStreamer backend initializes on a worker"
        )
    )]
    fn canceled_and_finished(&self) -> bool {
        self.result.is_none() && self.finished.load(std::sync::atomic::Ordering::Acquire)
    }
}

impl<T: Send + 'static> Initialization<T> {
    #[cfg_attr(
        not(feature = "audio"),
        allow(
            dead_code,
            reason = "only the GStreamer backend initializes on a worker"
        )
    )]
    fn spawn(initialize: impl FnOnce() -> Result<T, String> + Send + 'static) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_finished = finished.clone();
        let spawned = ui::rt::spawn_thread("music-audio-init", move || {
            // Ensure completion is visible even if a native wrapper panics.
            struct Finish(std::sync::Arc<std::sync::atomic::AtomicBool>);
            impl Drop for Finish {
                fn drop(&mut self) {
                    self.0.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            let _finish = Finish(worker_finished);
            // A canceled receiver disposes the backend here, on this worker.
            drop(sender.send(initialize()));
        });
        if let Err(error) = spawned {
            let (sender, receiver) = std::sync::mpsc::channel();
            let _ = sender.send(Err(format!("cannot start audio initialization: {error}")));
            finished.store(true, std::sync::atomic::Ordering::Release);
            return Self {
                result: Some(receiver),
                finished,
            };
        }
        Self {
            result: Some(receiver),
            finished,
        }
    }
}

/// Where decoded audio goes.
#[cfg(feature = "audio")]
#[derive(Clone)]
pub enum AudioOutput {
    /// The desktop's sound server, through `autoaudiosink`.
    System,
    /// An `appsink` that records the PCM reaching it and counts what it
    /// renders. A test observes the decoded audio itself this way, and needs
    /// no sound server or speaker.
    Capture(PcmCapture),
}

#[cfg(feature = "audio")]
impl AudioOutput {
    /// A unit test captures by default so that its playback is observable and
    /// silent; the application always plays through the system.
    fn for_build() -> Self {
        if cfg!(test) {
            Self::Capture(PcmCapture::default())
        } else {
            Self::System
        }
    }
}

/// What a capturing output has received so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CapturedPcm {
    /// Buffers of decoded audio that reached the sink, prerolled or played.
    pub buffers: u64,
    /// Buffers the sink rendered on the pipeline clock: audio that a speaker
    /// would have played, as opposed to a paused pipeline's preroll.
    pub rendered: u64,
    /// Signed 16-bit samples across every channel.
    pub samples: u64,
    /// The largest absolute sample value seen; zero means only silence arrived.
    pub peak: u16,
}

/// Shared record of the PCM a [`AudioOutput::Capture`] sink received, with a
/// condition variable so a reader can wait for audio instead of sleeping.
#[derive(Clone, Default)]
pub struct PcmCapture(std::sync::Arc<(std::sync::Mutex<CapturedPcm>, std::sync::Condvar)>);

impl PcmCapture {
    #[cfg_attr(
        not(feature = "audio"),
        allow(dead_code, reason = "only the native sink records")
    )]
    fn record(&self, bytes: &[u8]) {
        let (state, changed) = &*self.0;
        let mut pcm = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pcm.buffers += 1;
        for sample in bytes.chunks_exact(2) {
            if let [low, high] = *sample {
                let value = i16::from_le_bytes([low, high]);
                pcm.peak = pcm.peak.max(value.unsigned_abs());
                pcm.samples += 1;
            }
        }
        drop(pcm);
        changed.notify_all();
    }

    #[cfg_attr(
        not(feature = "audio"),
        allow(dead_code, reason = "only the native sink renders")
    )]
    fn note_rendered(&self) {
        let (state, changed) = &*self.0;
        state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rendered += 1;
        changed.notify_all();
    }

    pub fn snapshot(&self) -> CapturedPcm {
        *self
            .0
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait until more audio arrives or is rendered, or `timeout` passes,
    /// returning the latest record either way.
    pub fn wait_for_more(&self, timeout: std::time::Duration) -> CapturedPcm {
        let (state, changed) = &*self.0;
        let pcm = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let seen = *pcm;
        let (pcm, _) = changed
            .wait_timeout_while(pcm, timeout, |pcm| *pcm == seen)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *pcm
    }
}

/// The UI-owned audio handle. Construction and all observations are free of
/// native initialization. The first requested source starts a background job;
/// only the latest transport intent is applied when the backend arrives.
pub struct AudioEngine {
    #[cfg(feature = "spotify-native")]
    native: Option<spotify_audio::NativeAudio>,
    backend: Option<AudioBackend>,
    initialization: Option<Initialization<AudioBackend>>,
    current_uri: Option<String>,
    fallback: Option<String>,
    volume: f32,
    want_play: bool,
    pending_seek: Option<u64>,
    seek_started: Option<ui::time::Instant>,
    preroll_started: Option<ui::time::Instant>,
    initialization_failed: bool,
    initialization_error: std::sync::Mutex<Option<String>>,
    #[cfg(feature = "audio")]
    output: AudioOutput,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        #[cfg(feature = "audio")]
        {
            Self::with_output(AudioOutput::for_build())
        }
        #[cfg(not(feature = "audio"))]
        Self::unconfigured()
    }

    /// An engine whose pipelines send decoded audio to `output`.
    #[cfg(feature = "audio")]
    pub fn with_output(output: AudioOutput) -> Self {
        Self {
            output,
            ..Self::unconfigured()
        }
    }

    /// The capture this engine's pipelines record into, when it has one.
    #[cfg(feature = "audio")]
    pub fn capture(&self) -> Option<&PcmCapture> {
        match &self.output {
            AudioOutput::Capture(capture) => Some(capture),
            AudioOutput::System => None,
        }
    }

    fn unconfigured() -> Self {
        Self {
            #[cfg(feature = "spotify-native")]
            native: None,
            backend: None,
            initialization: None,
            current_uri: None,
            fallback: None,
            volume: 0.8,
            want_play: false,
            pending_seek: None,
            seek_started: None,
            preroll_started: None,
            initialization_failed: false,
            initialization_error: std::sync::Mutex::new(None),
            #[cfg(feature = "audio")]
            output: AudioOutput::System,
        }
    }

    /// Start an independently authorized native Spotify transport, never a URL resolver.
    /// # Errors
    /// Native support is absent or the worker could not start.
    pub fn load_spotify(&mut self, uri: &str, account: &str) -> Result<(), String> {
        self.stop();
        #[cfg(feature = "spotify-native")]
        {
            let native = spotify_audio::NativeAudio::start(
                uri.to_owned(),
                account.to_owned(),
                self.spotify_output(),
            )?;
            native.volume(self.volume);
            self.current_uri = Some(uri.to_owned());
            self.native = Some(native);
            Ok(())
        }
        #[cfg(not(feature = "spotify-native"))]
        {
            let _ = (uri, account);
            Err("This build does not include native Spotify playback".into())
        }
    }

    /// Safe, device-local diagnostics for the resource action door.
    pub fn spotify_startup_stage(&self) -> Option<&'static str> {
        #[cfg(feature = "spotify-native")]
        {
            self.native
                .as_ref()
                .map(spotify_audio::NativeAudio::startup_stage)
        }
        #[cfg(not(feature = "spotify-native"))]
        {
            None
        }
    }

    pub fn prepare_spotify_next(&self, uri: Option<&str>) {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            native.prepare_next(uri);
        }
        #[cfg(not(feature = "spotify-native"))]
        let _ = uri;
    }

    pub fn take_spotify_transition(&mut self) -> Option<String> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            let uri = native.take_transition()?;
            self.current_uri = Some(uri.clone());
            return Some(uri);
        }
        None
    }

    #[cfg(feature = "spotify-native")]
    fn spotify_output(&self) -> spotify_audio::Output {
        match &self.output {
            AudioOutput::System => spotify_audio::Output::System,
            AudioOutput::Capture(capture) => {
                let pcm = capture.clone();
                let rendered = capture.clone();
                spotify_audio::Output::Capture(spotify_audio::CaptureOutput {
                    on_pcm: std::sync::Arc::new(move |bytes| pcm.record(bytes)),
                    on_rendered: std::sync::Arc::new(move || rendered.note_rendered()),
                })
            }
        }
    }

    pub fn load(&mut self, uri: &str) {
        self.load_with_fallback(uri, None);
    }

    pub fn load_with_fallback(&mut self, uri: &str, fallback: Option<&str>) {
        #[cfg(feature = "spotify-native")]
        {
            self.native = None;
        }
        self.current_uri = Some(uri.to_owned());
        self.fallback = fallback.map(str::to_owned);
        self.want_play = false;
        self.pending_seek = None;
        self.seek_started = None;
        self.preroll_started = None;
        self.initialization_failed = false;
        if let Ok(error) = self.initialization_error.get_mut() {
            *error = None;
        }
        if let Some(backend) = &mut self.backend {
            backend.load_with_fallback(uri, fallback);
            // Preroll a replacement too: a queued seek needs PAUSED before
            // Play, and a paused handoff must not leave its pipeline in NULL.
            backend.pause();
        } else {
            self.start_initialization();
        }
    }

    fn start_initialization(&mut self) {
        #[cfg(feature = "audio")]
        {
            if self
                .initialization
                .as_ref()
                .is_some_and(Initialization::canceled_and_finished)
            {
                self.initialization = None;
            }
            if self.initialization.is_none() && !self.initialization_failed {
                let output = self.output.clone();
                self.initialization = Some(Initialization::spawn(move || {
                    AudioBackend::try_new(&output)
                }));
            }
        }
        #[cfg(not(feature = "audio"))]
        if self.initialization.is_none() && !self.initialization_failed && element_playback() {
            self.initialization = Some(Initialization::ready(AudioBackend::try_new()));
        }
    }

    /// Poll only while a source needs initialization or a deferred seek. Stop
    /// cancels its receiver and does not keep an idle window repainting.
    pub fn needs_poll(&self) -> bool {
        Self::is_live()
            && self.current_uri.is_some()
            && !self.initialization_failed
            && (self.backend.is_none()
                || self.pending_seek.is_some()
                || self.frames_rendered() == 0)
    }

    /// Whether governed native controls are still being applied by the worker.
    /// Callers can distinguish accepted transport intent from settled output.
    pub fn is_control_pending(&self) -> bool {
        #[cfg(feature = "audio")]
        {
            self.backend
                .as_ref()
                .and_then(|backend| backend.inner.controls.as_ref())
                .is_some_and(|controls| !controls.ready())
        }
        #[cfg(not(feature = "audio"))]
        {
            false
        }
    }

    pub fn play(&mut self) {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            self.want_play = true;
            native.play();
            return;
        }
        self.want_play = true;
        if self.pending_seek.is_none()
            && let Some(backend) = &mut self.backend
        {
            backend.play();
        }
    }

    pub fn pause(&mut self) {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            self.want_play = false;
            native.pause();
            return;
        }
        self.want_play = false;
        if let Some(backend) = &mut self.backend {
            backend.pause();
        }
    }

    pub fn stop(&mut self) {
        #[cfg(feature = "spotify-native")]
        {
            self.native = None;
        }
        self.current_uri = None;
        self.fallback = None;
        self.want_play = false;
        self.pending_seek = None;
        self.seek_started = None;
        self.preroll_started = None;
        self.initialization_failed = false;
        if let Ok(error) = self.initialization_error.get_mut() {
            *error = None;
        }
        if let Some(job) = &mut self.initialization {
            job.cancel();
        }
        if let Some(backend) = &mut self.backend {
            backend.stop();
        }
    }

    pub fn seek_ms(&mut self, ms: u64) {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            native.seek(ms);
            return;
        }
        if !Self::is_live() || self.current_uri.is_none() {
            return;
        }
        self.pending_seek = Some(ms);
        self.seek_started = None;
        self.apply_seek();
    }

    fn apply_seek(&mut self) {
        if let Some(ms) = self.pending_seek
            && let Some(backend) = &mut self.backend
        {
            // Network staging has its own deadlines. The preroll deadline
            // starts only after there is a local source to seek through.
            #[cfg(all(feature = "audio", feature = "live"))]
            if backend
                .inner
                .pending
                .as_ref()
                .is_some_and(|pending| !pending.started)
            {
                return;
            }
            let started = self.seek_started.get_or_insert_with(ui::time::now);
            let outcome = backend.try_seek_ms(ms).or_else(|| {
                (started.elapsed() >= std::time::Duration::from_secs(5))
                    .then(|| Err("audio source did not become seekable within five seconds".into()))
            });
            if let Some(outcome) = outcome {
                self.pending_seek = None;
                self.seek_started = None;
                match outcome {
                    Ok(()) if self.want_play => backend.play(),
                    Ok(()) => {}
                    Err(error) => {
                        self.initialization_failed = true;
                        if let Ok(message) = self.initialization_error.get_mut() {
                            *message = Some(error);
                        }
                    }
                }
            }
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            self.volume = volume.clamp(0.0, 1.0);
            native.volume(self.volume);
            return;
        }
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(backend) = &mut self.backend {
            backend.set_volume(self.volume);
        }
    }
    pub fn volume(&self) -> f32 {
        self.volume
    }
    pub fn current_uri(&self) -> Option<&str> {
        self.current_uri.as_deref()
    }
    pub fn position_ms(&self) -> Option<u64> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.position();
        }
        self.backend.as_ref()?.position_ms()
    }
    pub fn duration_ms(&self) -> Option<u64> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.duration_ms();
        }
        self.backend.as_ref()?.duration_ms()
    }
    pub fn has_stream(&self) -> bool {
        Self::is_live() && self.current_uri.is_some()
    }
    pub fn is_live() -> bool {
        cfg!(feature = "audio") || element_playback()
    }
    pub fn is_playing_real(&self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.playing();
        }
        self.backend
            .as_ref()
            .is_some_and(AudioBackend::is_playing_real)
    }

    /// A settled PLAYING state, excluding a pipeline merely transitioning there.
    #[cfg_attr(
        not(feature = "audio"),
        allow(dead_code, reason = "only the GStreamer backend confirms playback")
    )]
    pub(crate) fn is_playing_confirmed(&self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return self.want_play && native.playing();
        }
        self.want_play
            && self.current_uri.is_some()
            && self.pending_seek.is_none()
            && self
                .backend
                .as_ref()
                .is_some_and(AudioBackend::is_playing_confirmed)
    }

    pub fn pump(&mut self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.pump();
        }
        if self.current_uri.is_none() {
            return false;
        }
        if self.backend.is_none() && !self.initialization_failed {
            self.start_initialization();
            if let Some(result) = self.initialization.as_ref().and_then(Initialization::poll) {
                self.initialization = None;
                match result {
                    Ok(mut backend) => {
                        backend.set_volume(self.volume);
                        if let Some(uri) = &self.current_uri {
                            backend.load_with_fallback(uri, self.fallback.as_deref());
                            if self.want_play && self.pending_seek.is_none() {
                                backend.play();
                            } else {
                                backend.pause();
                            }
                        }
                        self.backend = Some(backend);
                    }
                    Err(error) => {
                        self.initialization_failed = true;
                        if let Ok(message) = self.initialization_error.get_mut() {
                            *message = Some(error);
                        }
                    }
                }
            }
        }
        let failed = self.backend.as_mut().is_some_and(AudioBackend::pump);
        if !failed {
            self.apply_seek();
            if self.backend.is_some() && self.frames_rendered() == 0 {
                #[cfg(all(feature = "audio", feature = "live"))]
                if self.backend.as_ref().is_some_and(|backend| {
                    backend
                        .inner
                        .pending
                        .as_ref()
                        .is_some_and(|pending| !pending.started)
                }) {
                    return self.initialization_failed;
                }
                let started = self.preroll_started.get_or_insert_with(ui::time::now);
                if started.elapsed() >= std::time::Duration::from_secs(5) {
                    self.initialization_failed = true;
                    if let Ok(message) = self.initialization_error.get_mut() {
                        *message = Some(
                            "audio decoder did not produce a buffer within five seconds".into(),
                        );
                    }
                }
            }
        }
        self.initialization_failed || failed
    }

    pub fn take_error(&self) -> Option<String> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.error();
        }
        self.initialization_error
            .lock()
            .ok()
            .and_then(|mut error| error.take())
            .or_else(|| self.backend.as_ref().and_then(AudioBackend::take_error))
    }
    pub fn reached_eos(&self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.eos();
        }
        self.backend.as_ref().is_some_and(AudioBackend::reached_eos)
    }
    pub fn staging_complete(&self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.frames() > 0;
        }
        self.backend.as_ref().map_or_else(
            || self.current_uri.is_none(),
            AudioBackend::staging_complete,
        )
    }
    pub fn staging_finished(&self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.frames() > 0;
        }
        self.backend.as_ref().map_or_else(
            || self.current_uri.is_none(),
            AudioBackend::staging_finished,
        )
    }
    pub fn staged_progress(&self) -> Option<(u64, Option<u64>)> {
        self.backend.as_ref()?.staged_progress()
    }
    pub fn frames_rendered(&self) -> u64 {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.frames();
        }
        self.backend
            .as_ref()
            .map_or(0, AudioBackend::frames_rendered)
    }
    pub fn actual_audio_sink_name(&self) -> Option<String> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.sink();
        }
        self.backend.as_ref()?.actual_audio_sink_name()
    }
    pub fn audio_sink_is_real(&self) -> Option<bool> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.sink().map(|name| !name.contains("fakesink"));
        }
        self.backend.as_ref()?.audio_sink_is_real()
    }

    /// Where a source stands on its way to audio, for a test's failure message.
    #[cfg(test)]
    #[cfg(feature = "audio")]
    pub fn diagnostics(&self) -> String {
        use gstreamer::prelude::*;
        let pipeline = self.backend.as_ref().map(|backend| {
            let (result, current, pending) =
                backend.inner.playbin.state(gstreamer::ClockTime::ZERO);
            format!(
                "{result:?} {current:?}->{pending:?}, sink {:?}",
                backend.inner.sink.as_ref().map(GstObjectExt::name)
            )
        });
        format!(
            "uri {:?}, initializing {}, pipeline {pipeline:?}, frames {}, want_play {}, \
             pending_seek {:?}, preroll {:?}, failed {}, capture {:?}",
            self.current_uri,
            self.initialization.is_some(),
            self.frames_rendered(),
            self.want_play,
            self.pending_seek,
            self.preroll_started.map(|started| started.elapsed()),
            self.initialization_failed,
            self.capture().map(PcmCapture::snapshot),
        )
    }

    /// Bounded test/probe helper; production frames call `pump` instead.
    #[cfg(feature = "audio")]
    pub fn wait_until_playing(&mut self, timeout: std::time::Duration) -> bool {
        let deadline = ui::time::now() + timeout;
        loop {
            if self.pump() {
                return false;
            }
            if self.backend.as_ref().is_some_and(|backend| {
                use gstreamer::prelude::*;
                backend.inner.playbin.current_state() == gstreamer::State::Playing
            }) {
                return true;
            }
            if ui::time::now() >= deadline {
                return false;
            }
            if ui::rt::sleep_thread(std::time::Duration::from_millis(5)).is_err() {
                return false;
            }
        }
    }
}

/// Ready native backend, owned by the initialization worker until handoff.
struct AudioBackend {
    inner: Inner,
    /// Last requested URI (for diagnostics / the silent stub).
    current_uri: Option<String>,
    volume: f32,
}

/// A device-local bounded-range transfer for an offline cache request. It
/// exposes only aggregate progress and a terminal result; paths and bytes stay
/// within the storage/audio layers.
pub struct OfflineDownload {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fetched: std::sync::Arc<std::sync::atomic::AtomicU64>,
    expected: std::sync::Arc<std::sync::atomic::AtomicU64>,
    result: std::sync::mpsc::Receiver<Result<(), String>>,
}

impl OfflineDownload {
    /// Start downloading a resolved source into the device-local cache.
    ///
    /// # Errors
    ///
    /// Returns an error when this build cannot perform network transfers or
    /// when the local transfer thread cannot be created.
    pub fn start(
        track_id: String,
        uri: String,
        fallback: Option<String>,
        cache_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        #[cfg(all(feature = "audio", feature = "live"))]
        {
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let fetched = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let expected = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let (tx, result) = std::sync::mpsc::channel();
            let root = cache_root;
            let partial = root.join("partials").join(format!(
                "{}-{}.part",
                std::process::id(),
                next_stream_id()
            ));
            let parent = partial
                .parent()
                .ok_or_else(|| "offline cache path has no parent".to_string())?;
            crate::files::create_dir_all(parent).map_err(|error| error.to_string())?;
            let cancel_thread = cancel.clone();
            let fetched_thread = fetched.clone();
            let expected_thread = expected.clone();
            ui::rt::spawn_thread("tilo-music-offline-dl", move || {
                use std::sync::atomic::Ordering::Relaxed;
                let outcome = (|| -> Result<(), String> {
                    let mut file = crate::files::Growing::create(&partial)
                        .map_err(|error| error.to_string())?;
                    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let mut wrote = download_into(
                        &uri,
                        &mut file,
                        &ready,
                        &fetched_thread,
                        &expected_thread,
                        &cancel_thread,
                    );
                    if wrote == 0
                        && let Some(fallback) = fallback
                    {
                        fetched_thread.store(0, Relaxed);
                        expected_thread.store(0, Relaxed);
                        file.reset().map_err(|error| error.to_string())?;
                        wrote = download_into(
                            &fallback,
                            &mut file,
                            &ready,
                            &fetched_thread,
                            &expected_thread,
                            &cancel_thread,
                        );
                    }
                    let declared = expected_thread.load(Relaxed);
                    if cancel_thread.load(Relaxed) {
                        return Err("offline download cancelled".into());
                    }
                    if wrote == 0 || (declared > 0 && wrote < declared) {
                        return Err("source did not deliver a complete audio file".into());
                    }
                    std::io::Write::flush(&mut file).map_err(|error| error.to_string())?;
                    crate::storage::install_completed_media_file(
                        &root,
                        &track_id,
                        &partial,
                        crate::storage::media_cache_quota(),
                    )
                    .map_err(|error| error.to_string())
                })();
                if outcome.is_err() {
                    crate::files::remove(&partial);
                }
                let _ = tx.send(outcome);
            })
            .map_err(|error| error.to_string())?;
            Ok(Self {
                cancel,
                fetched,
                expected,
                result,
            })
        }
        #[cfg(not(all(feature = "audio", feature = "live")))]
        {
            let _ = (track_id, uri, fallback, cache_root);
            Err("this Music build cannot download offline media".into())
        }
    }
    pub fn cancel(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    #[must_use]
    pub fn progress(&self) -> Option<f32> {
        let expected = self.expected.load(std::sync::atomic::Ordering::Relaxed);
        (expected > 0).then(|| {
            (self.fetched.load(std::sync::atomic::Ordering::Relaxed) as f32 / expected as f32)
                .clamp(0.0, 1.0)
        })
    }
    pub fn take_result(&self) -> Option<Result<(), String>> {
        self.result.try_recv().ok()
    }
}

#[cfg(feature = "audio")]
struct Inner {
    playbin: gstreamer::Element,
    controls: Option<pipeline_controls::Controls>,
    governed_output: Option<std::sync::Arc<media::audio_output::gst::Bridge>>,
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
    /// Set by the download thread when it stops fetching — whether because the
    /// whole file landed *or* because the source refused a range. Distinct from
    /// `ready` (first chunk) and, crucially, from "the file is complete": a
    /// range-gated source ends the fetch early, and asking `done` alone reports
    /// that as a finished download. Whether the bytes are all here is
    /// `fetched`/`expected`, not this.
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Bytes written to `path` so far, published live by the download thread.
    fetched: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// The source's declared HTTP total (or `clen=` hint), or 0 when it did not
    /// declare one. With `fetched` this is the only honest answer to "is a seek
    /// to a late position backed by bytes on disk?".
    expected: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Whether the transport wants to be playing once the file is ready.
    want_play: bool,
    /// Whether playback has already been switched onto the temp file.
    started: bool,
}

/// Without `GStreamer`, the platform's own player where it has one (the page's
/// media element, through `media::MediaEngine`); elsewhere a silent stub.
#[cfg(not(feature = "audio"))]
struct Inner {
    engine: Option<media::MediaEngine>,
    duration_known: bool,
    eos: bool,
    error: std::sync::Mutex<Option<String>>,
}

/// Whether the platform plays media without `GStreamer` (the web).
fn element_playback() -> bool {
    !cfg!(feature = "audio")
        && media::availability(media::Feature::Playback) == media::Availability::Available
}

impl AudioBackend {
    /// Construct the actual pipeline on the initialization worker.
    #[cfg_attr(
        not(feature = "audio"),
        allow(
            dead_code,
            clippy::unnecessary_wraps,
            reason = "the silent build never starts a backend"
        )
    )]
    fn try_new(#[cfg(feature = "audio")] output: &AudioOutput) -> Result<Self, String> {
        #[cfg(feature = "audio")]
        let inner = {
            use gstreamer::prelude::*;
            gstreamer::init()
                .map_err(|error| format!("GStreamer initialization failed: {error}"))?;
            let playbin = gstreamer::ElementFactory::make("playbin")
                .build()
                .map_err(|error| format!("could not create GStreamer playbin: {error}"))?;
            let frames = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            // Route audio through an explicit **real** sink chain, so the
            // output is audible and verifiable. `autoaudiosink` selects
            // the concrete system sink (pipewire/pulse/alsa); the pad
            // probe lets us confirm real buffers flow.
            let (sink, governed_output) =
                if matches!(output, AudioOutput::System) && media::audio_output::is_governed() {
                    let (bin, bridge) = media::audio_output::gst::host_clocked_sink_bin(
                        "tilo_audiosink",
                        "tilo_governed_gain",
                    )?;
                    let sink = bin.by_name("tilo_audiosink");
                    playbin.set_property("audio-sink", &bin);
                    (sink, Some(bridge))
                } else {
                    (build_audio_sink(&playbin, &frames, output), None)
                };
            let errored = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            // Defensive: never leave the transport muted, and start at a
            // sane volume so "no sound" is never a stuck property.
            playbin.set_property("mute", false);
            playbin.set_property("volume", 0.8f64);
            let error_msg = std::sync::Arc::new(std::sync::Mutex::new(None));
            let eos = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let controls = governed_output
                .as_ref()
                .map(|output| {
                    pipeline_controls::Controls::spawn(pipeline_controls::Pipeline {
                        element: playbin.clone(),
                        output: output.clone(),
                        frames: frames.clone(),
                        eos: eos.clone(),
                        failed: errored.clone(),
                        error: error_msg.clone(),
                    })
                })
                .transpose()?;
            Inner {
                playbin,
                controls,
                governed_output,
                sink,
                frames,
                errored,
                error_msg,
                eos,
                #[cfg(feature = "live")]
                pending: None,
            }
        };
        #[cfg(not(feature = "audio"))]
        let inner = Inner {
            engine: element_playback().then(media::MediaEngine::new),
            duration_known: false,
            eos: false,
            error: std::sync::Mutex::new(None),
        };
        Ok(Self {
            inner,
            current_uri: None,
            volume: 0.8,
        })
    }

    /// Load a URI with a secondary source the staging download
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
            if let Some(controls) = &self.inner.controls {
                controls.source(None, self.volume);
            } else {
                let _ = self.inner.playbin.set_state(gstreamer::State::Null);
                if let Some(output) = &self.inner.governed_output {
                    output.reset();
                }
            }
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

            if let Some(controls) = &self.inner.controls {
                controls.source(Some(uri), self.volume);
            } else {
                self.inner.playbin.set_property("uri", uri);
                self.inner.playbin.set_property("mute", false);
                self.inner
                    .playbin
                    .set_property("volume", f64::from(self.volume));
            }
        }
        #[cfg(not(feature = "audio"))]
        if let Some(engine) = &mut self.inner.engine {
            self.inner.duration_known = false;
            self.inner.eos = false;
            engine.open(media::Source::Remote {
                url: uri.to_owned(),
            });
            engine.set_volume(self.volume);
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
            if let Some(controls) = &self.inner.controls {
                controls.playing(true);
            } else {
                let _ = self.inner.playbin.set_state(gstreamer::State::Playing);
            }
        }
        #[cfg(not(feature = "audio"))]
        if let Some(engine) = &mut self.inner.engine {
            engine.play();
        }
    }

    pub fn pause(&mut self) {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            #[cfg(feature = "live")]
            if let Some(p) = &mut self.inner.pending {
                p.want_play = false;
                if !p.started {
                    return;
                }
            }
            if let Some(controls) = &self.inner.controls {
                controls.playing(false);
            } else {
                let _ = self.inner.playbin.set_state(gstreamer::State::Paused);
            }
        }
        #[cfg(not(feature = "audio"))]
        if let Some(engine) = &mut self.inner.engine {
            engine.pause();
        }
    }

    /// Stop and tear the pipeline down to `Null` (release the decoder + sink).
    pub fn stop(&mut self) {
        self.current_uri = None;
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            if let Some(controls) = &self.inner.controls {
                controls.source(None, self.volume);
            } else {
                let _ = self.inner.playbin.set_state(gstreamer::State::Null);
                if let Some(output) = &self.inner.governed_output {
                    output.reset();
                }
            }
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
        if let Some(engine) = &mut self.inner.engine {
            engine.pause();
            self.inner.eos = false;
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
    #[cfg_attr(
        feature = "audio",
        allow(
            clippy::needless_pass_by_ref_mut,
            reason = "the element backend seeks through its engine"
        )
    )]
    fn try_seek_ms(&mut self, ms: u64) -> Option<Result<(), String>> {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            if let Some(controls) = &self.inner.controls {
                return controls.seek(ms).then_some(Ok(()));
            }
            if !matches!(
                self.inner.playbin.current_state(),
                gstreamer::State::Paused | gstreamer::State::Playing
            ) {
                return None;
            }
            let mut seeking = gstreamer::query::Seeking::new(gstreamer::Format::Time);
            if self.inner.playbin.query(&mut seeking) && !seeking.result().0 {
                return Some(Err("this audio source does not support seeking".into()));
            }
            let target = match self.duration_ms() {
                // Never seek to (or past) the very end — keep a 250 ms guard so a
                // scrub to ~100% resumes just before the end rather than EOS-ing.
                Some(dur) if dur > 0 => ms.min(dur.saturating_sub(250)),
                _ => ms,
            };
            Some(
                self.inner
                    .playbin
                    .seek_simple(
                        gstreamer::SeekFlags::FLUSH | gstreamer::SeekFlags::KEY_UNIT,
                        gstreamer::ClockTime::from_mseconds(target),
                    )
                    .map_err(|error| format!("audio source rejected the seek: {error}")),
            )
        }
        #[cfg(not(feature = "audio"))]
        {
            let engine = self.inner.engine.as_mut()?;
            let target = engine
                .duration_ms()
                .filter(|d| *d > 0)
                .map_or(ms, |d| ms.min(d.saturating_sub(250)));
            // The element seeks asynchronously; accepting the request is the
            // answer.
            engine.seek_ms(target);
            Some(Ok(()))
        }
    }

    /// Set output volume `0.0..=1.0`.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            if let Some(controls) = &self.inner.controls {
                controls.volume(self.volume);
            } else {
                self.inner
                    .playbin
                    .set_property("volume", f64::from(self.volume));
            }
        }
        #[cfg(not(feature = "audio"))]
        if let Some(engine) = &mut self.inner.engine {
            engine.set_volume(self.volume);
        }
    }

    /// Query the pipeline's current playhead in milliseconds, when the live
    /// `GStreamer` backend is running and a position is available. The UI uses
    /// this to drive a *real* (not simulated) playhead while audio plays;
    /// `None` falls back to the local simulated playhead.
    pub fn position_ms(&self) -> Option<u64> {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            if self
                .inner
                .controls
                .as_ref()
                .is_some_and(|controls| !controls.ready())
            {
                return None;
            }
            self.inner
                .playbin
                .query_position::<gstreamer::ClockTime>()
                .map(gstreamer::ClockTime::mseconds)
        }
        #[cfg(not(feature = "audio"))]
        {
            self.inner
                .engine
                .as_ref()
                .filter(|_| self.current_uri.is_some())
                .map(media::MediaEngine::position_ms)
        }
    }

    /// Query the pipeline's total duration in milliseconds, when known.
    pub fn duration_ms(&self) -> Option<u64> {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            if self
                .inner
                .controls
                .as_ref()
                .is_some_and(|controls| !controls.ready())
            {
                return None;
            }
            self.inner
                .playbin
                .query_duration::<gstreamer::ClockTime>()
                .map(gstreamer::ClockTime::mseconds)
        }
        #[cfg(not(feature = "audio"))]
        {
            self.inner
                .engine
                .as_ref()
                .filter(|_| self.inner.duration_known)
                .and_then(media::MediaEngine::duration_ms)
        }
    }

    /// Whether the real pipeline has actually reached (or is heading to) the
    /// `PLAYING` state. The now-playing indicator reads this so it only reflects
    /// genuine playback, never a fake timer. Always `false` for the silent stub.
    pub fn is_playing_real(&self) -> bool {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            if self.current_uri.is_none()
                || self
                    .inner
                    .controls
                    .as_ref()
                    .is_some_and(|controls| !controls.ready())
            {
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
            self.current_uri.is_some()
                && self
                    .inner
                    .engine
                    .as_ref()
                    .is_some_and(media::MediaEngine::is_playing)
        }
    }

    #[cfg_attr(
        not(feature = "audio"),
        allow(dead_code, reason = "only the GStreamer backend confirms playback")
    )]
    fn is_playing_confirmed(&self) -> bool {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            self.current_uri.is_some()
                && self
                    .inner
                    .controls
                    .as_ref()
                    .is_none_or(pipeline_controls::Controls::ready)
                && !self.reached_eos()
                && !self
                    .inner
                    .errored
                    .load(std::sync::atomic::Ordering::Relaxed)
                && self.inner.playbin.state(gstreamer::ClockTime::ZERO).1
                    == gstreamer::State::Playing
        }
        #[cfg(not(feature = "audio"))]
        {
            self.is_playing_real() && !self.inner.eos
        }
    }

    /// Service the `GStreamer` bus (non-blocking): drain pending messages so they
    /// don't accumulate, and latch a fatal error so the transport stops
    /// pretending to play. Called once per frame from the app's tick. Also
    /// returns whether an error is currently latched, for diagnostics.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over every backend event, in order"
    )]
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
                    let has_bytes = crate::files::metadata(&path).is_ok_and(|m| m.len > 0);
                    if has_bytes {
                        let uri = match media::source::path_to_file_uri(&path) {
                            Ok(uri) => uri,
                            Err(error) => {
                                self.inner
                                    .errored
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                if let Ok(mut message) = self.inner.error_msg.lock() {
                                    *message = Some(format!("cannot open staged audio: {error}"));
                                }
                                if let Some(pending) = &mut self.inner.pending {
                                    pending.started = true;
                                }
                                return true;
                            }
                        };
                        let bytes = crate::files::metadata(&path).map_or(0, |m| m.len);
                        tracing::info!(target: "music::audio", %uri, bytes, want_play = want, "staged file ready — switching playbin to local file and rolling");
                        if let Some(controls) = &self.inner.controls {
                            controls.source(Some(&uri), self.volume);
                            controls.playing(want);
                        } else {
                            let _ = self.inner.playbin.set_state(gstreamer::State::Null);
                            self.inner.playbin.set_property("uri", &uri);
                            self.inner.playbin.set_property("mute", false);
                            self.inner
                                .playbin
                                .set_property("volume", f64::from(self.volume));
                            let _ = self.inner.playbin.set_state(if want {
                                gstreamer::State::Playing
                            } else {
                                gstreamer::State::Paused
                            });
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
            if self
                .inner
                .controls
                .as_ref()
                .is_some_and(|controls| !controls.ready())
            {
                return false;
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
            if let Some(error) = self
                .inner
                .governed_output
                .as_ref()
                .and_then(|output| output.error())
            {
                self.inner
                    .errored
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                if let Ok(mut message) = self.inner.error_msg.lock() {
                    *message = Some(error);
                }
            }
            self.inner
                .errored
                .load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(feature = "audio"))]
        {
            let Some(engine) = &mut self.inner.engine else {
                return false;
            };
            let mut failed = false;
            for event in engine.poll() {
                match event {
                    media::MediaEvent::DurationKnown { .. } => self.inner.duration_known = true,
                    media::MediaEvent::Ended => self.inner.eos = true,
                    media::MediaEvent::Error(message) => {
                        failed = true;
                        if let Ok(mut error) = self.inner.error.lock() {
                            *error = Some(message);
                        }
                    }
                    media::MediaEvent::Position { .. }
                    | media::MediaEvent::Buffering { .. }
                    | media::MediaEvent::MissingDecoder { .. } => {}
                }
            }
            failed
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
            self.inner.error.lock().ok().and_then(|mut e| e.take())
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
            if self
                .inner
                .controls
                .as_ref()
                .is_some_and(|controls| !controls.ready())
            {
                return false;
            }
            self.inner.eos.load(std::sync::atomic::Ordering::Relaxed)
                && self
                    .inner
                    .governed_output
                    .as_ref()
                    .is_none_or(|output| output.drained())
        }
        #[cfg(not(feature = "audio"))]
        {
            self.inner.eos
        }
    }

    /// Whether the staging download (if any) has fetched the **entire** file, so
    /// a seek to a late position is backed by bytes on disk. `true` when there is
    /// no in-flight staging (a local `file://` uri, or nothing loaded).
    ///
    /// This asks the byte counts, not the download thread's exit. A range-gated
    /// source (`YouTube`'s SABR gating refuses every range past the first ~1 MB)
    /// ends the fetch early with a partial file, and the thread finishes exactly
    /// as it does on success — so reading "the download stopped" as "the file is
    /// complete" reports a ~1-minute stub as a whole track. Everything
    /// downstream then believes a seek to a late position is backed by bytes
    /// that are not there.
    ///
    /// An unknown length cannot establish completeness, even when fetching
    /// has stopped. Failed or rejected requests therefore stay incomplete.
    #[cfg_attr(
        not(all(feature = "audio", feature = "live")),
        allow(
            clippy::unused_self,
            reason = "only the streaming backend stages files"
        )
    )]
    pub fn staging_complete(&self) -> bool {
        #[cfg(all(feature = "audio", feature = "live"))]
        {
            use std::sync::atomic::Ordering::Relaxed;
            match &self.inner.pending {
                Some(p) => {
                    let expected = p.expected.load(Relaxed);
                    if expected == 0 {
                        return false;
                    }
                    p.fetched.load(Relaxed) >= expected
                }
                None => true,
            }
        }
        #[cfg(not(all(feature = "audio", feature = "live")))]
        {
            true
        }
    }

    /// Whether the staging download has stopped fetching, for any reason —
    /// the whole file landed, or the source refused a range. Pair it with
    /// [`Self::staging_complete`] to tell a finished download from a complete
    /// one; on its own it only says the bytes will not grow any further.
    #[cfg_attr(
        not(all(feature = "audio", feature = "live")),
        allow(
            clippy::unused_self,
            reason = "only the streaming backend stages files"
        )
    )]
    pub fn staging_finished(&self) -> bool {
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

    /// Bytes of the staged stream on disk so far, and the length the source
    /// declared (`None` when it declared none, or when nothing is staging).
    /// Reported so a caller can say *how far short* a refused fetch fell rather
    /// than only that it did.
    #[cfg_attr(
        not(all(feature = "audio", feature = "live")),
        allow(
            clippy::unused_self,
            reason = "only the streaming backend stages files"
        )
    )]
    pub fn staged_progress(&self) -> Option<(u64, Option<u64>)> {
        #[cfg(all(feature = "audio", feature = "live"))]
        {
            use std::sync::atomic::Ordering::Relaxed;
            let p = self.inner.pending.as_ref()?;
            let expected = p.expected.load(Relaxed);
            Some((p.fetched.load(Relaxed), (expected > 0).then_some(expected)))
        }
        #[cfg(not(all(feature = "audio", feature = "live")))]
        {
            None
        }
    }

    /// Number of decoded PCM buffers that have actually reached the audio sink.
    /// Non-zero proves audio is *rendering* to the sink — real audible output,
    /// not just a pipeline that reported `PLAYING`. Always `0` for the stub.
    pub fn frames_rendered(&self) -> u64 {
        #[cfg(feature = "audio")]
        {
            if self
                .inner
                .controls
                .as_ref()
                .is_some_and(|controls| !controls.ready())
            {
                return 0;
            }
            if let Some(output) = &self.inner.governed_output {
                return output.observation().rendered_frames;
            }
            self.inner.frames.load(std::sync::atomic::Ordering::Relaxed)
        }
        #[cfg(not(feature = "audio"))]
        {
            // The element reports no buffers; its metadata having loaded is
            // what proves the source decodes.
            u64::from(self.inner.duration_known)
        }
    }

    /// Factory name of the concrete audio sink `autoaudiosink` plugged
    /// (`pipewiresink` / `pulsesink` / `alsasink`), once it has resolved one
    /// (at READY+). `None` before then, without the `audio` feature, or if no
    /// explicit sink chain was built. Used to prove the live path renders to a
    /// **real** sink and never a `fakesink` — and to diagnose silence (a
    /// `fakesink` here means no real sink plugin is installed).
    #[cfg_attr(
        not(feature = "audio"),
        allow(clippy::unused_self, reason = "only `GStreamer` builds a sink chain")
    )]
    pub fn actual_audio_sink_name(&self) -> Option<String> {
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            if let Some(output) = &self.inner.governed_output {
                return output
                    .observation()
                    .ready
                    .then(|| "tilo-governed-pipewire".into());
            }
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
    #[cfg(test)]
    #[cfg(feature = "audio")]
    pub fn wait_until_playing(&self, timeout: std::time::Duration) -> bool {
        use gstreamer::prelude::*;
        let deadline = ui::time::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(ui::time::now());
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

    /// Begin staging an http(s) stream to a temp file via bounded reqwest ranges.
    /// Non-blocking: the fetch runs on its own thread; [`Self::pump`] switches
    /// playback onto the file once it is ready.
    #[cfg(all(feature = "audio", feature = "live"))]
    fn begin_stream_download(&mut self, uri: &str, fallback: Option<String>) {
        self.clear_pending();
        // Without a native temp dir the relative path fails to open, and the
        // staging reports the stream finished with nothing fetched.
        let path = tilo_paths::temp_dir().unwrap_or_default().join(format!(
            "tilo-music-stream-{}-{}.bin",
            std::process::id(),
            next_stream_id()
        ));
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fetched = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let expected = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        spawn_stream_download(
            uri.to_string(),
            fallback,
            path.clone(),
            ready.clone(),
            done.clone(),
            fetched.clone(),
            expected.clone(),
            cancel.clone(),
        );
        self.inner.pending = Some(Pending {
            path,
            ready,
            done,
            cancel,
            fetched,
            expected,
            want_play: false,
            started: false,
        });
    }

    /// Drop any staged stream, removing its temp file.
    #[cfg(all(feature = "audio", feature = "live"))]
    fn clear_pending(&mut self) {
        if let Some(p) = self.inner.pending.take() {
            p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            crate::files::remove(&p.path);
        }
    }
}

/// The HTTP `User-Agent`s of the player clients that sign the media URLs this
/// app plays: the two rustypipe resolves through (its default order with no
/// `BotGuard` is `[Ios, Tv]`) plus the `ANDROID_VR` client queried by the
/// Rust-native full-track resolver. We present the client UA
/// matching the URL's `c=` param on the media fetch to mirror the client that
/// signed it. The iOS/TV values mirror rustypipe's own client UAs (crate 0.11);
/// the Android VR value matches the direct full-track resolver.
#[cfg(all(feature = "audio", feature = "live"))]
const IOS_UA: &str =
    "com.google.ios.youtube/20.03.02 (iPhone16,2; U; CPU iOS 18_2_1 like Mac OS X)";
#[cfg(all(feature = "audio", feature = "live"))]
const TV_UA: &str = "Mozilla/5.0 (SMART-TV; Linux; Tizen 5.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/5.0 NativeTVAds Safari/538.1";
/// The client the Rust-native full-track resolver asks for. Without this arm such a
/// URL was fetched under the iOS UA — a client it was not signed for, which the
/// rule above exists to prevent.
#[cfg(all(feature = "audio", feature = "live"))]
const ANDROID_VR_UA: &str = crate::backend::android_vr::USER_AGENT;

/// Pick the `User-Agent` for fetching a media URL.
///
/// A signed **googlevideo** URL is presented with the client UA matching its
/// `c=` query param (`c=IOS`, `c=TV`, `c=ANDROID_VR`, …), mirroring the client
/// that signed it. Any other source (notably an Invidious `local` proxy) gets a
/// plain browser UA — those proxies expect a normal browser, not the iOS
/// `YouTube` app.
#[cfg(all(feature = "audio", feature = "live"))]
fn ua_for_uri(uri: &str) -> &'static str {
    if uri.contains("googlevideo.com") {
        let client = uri.split(['?', '&']).find_map(|kv| kv.strip_prefix("c="));
        return match client {
            Some(c) if c.starts_with("TV") => TV_UA,
            Some(c) if c.starts_with("ANDROID_VR") => ANDROID_VR_UA,
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

/// Retry a transient connection/server failure once. Authentication, access
/// restrictions and rate limits are returned unchanged, never retried.
#[cfg(all(feature = "audio", feature = "live"))]
fn request_range(
    client: &reqwest::blocking::Client,
    uri: &str,
    off: u64,
    end: u64,
    cancel: &std::sync::atomic::AtomicBool,
) -> Option<reqwest::blocking::Response> {
    for attempt in 0..2 {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        match client
            .get(uri)
            .header(reqwest::header::RANGE, format!("bytes={off}-{end}"))
            .send()
        {
            Ok(response) if response.status().is_server_error() && attempt == 0 => {}
            Ok(response) => return Some(response),
            Err(_) if attempt == 0 => {}
            Err(_) => return None,
        }
    }
    None
}

/// A range must identify exactly the bytes requested. Use HTTP metadata for
/// completeness even when the URL carries no provider-specific length hint.
#[cfg(all(feature = "audio", feature = "live"))]
fn response_range_length(
    response: &reqwest::blocking::Response,
    off: u64,
    end: u64,
    total: &mut Option<u64>,
) -> Option<u64> {
    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let value = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)?
            .to_str()
            .ok()?;
        let (range, size) = value.strip_prefix("bytes ")?.split_once('/')?;
        let (start, last) = range.split_once('-')?;
        let start = start.parse::<u64>().ok()?;
        let last = last.parse::<u64>().ok()?;
        let size = size.parse::<u64>().ok()?;
        if start != off
            || last < start
            || last > end
            || last >= size
            || total.is_some_and(|known| known != size)
        {
            return None;
        }
        *total = Some(size);
        let length = last - start + 1;
        return response
            .content_length()
            .is_none_or(|n| n == length)
            .then_some(length);
    }
    // A server may ignore Range only on the initial request, and only for a
    // complete body fitting the bound. Appending that response would corrupt it.
    let length = response.content_length()?;
    if off != 0 || length == 0 || length > end + 1 || total.is_some_and(|known| known != length) {
        return None;
    }
    *total = Some(length);
    Some(length)
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
    fetched: std::sync::Arc<std::sync::atomic::AtomicU64>,
    expected: std::sync::Arc<std::sync::atomic::AtomicU64>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering::Relaxed;
    let _ = ui::rt::spawn_thread("tilo-music-stream-dl", move || {
        let finish = || {
            if cancel.load(Relaxed) {
                crate::files::remove(&path);
            }
            ready.store(true, Relaxed);
            done.store(true, Relaxed);
        };
        if cancel.load(Relaxed) {
            return finish();
        }
        let Ok(mut file) = crate::files::Growing::create(&path) else {
            return finish();
        };
        // Primary source (the rangeable proxy). If it yields no bytes at all
        // — a broken/rate-limited public proxy — fall back to the direct
        // googlevideo URL so playback still works (at least its first
        // segment). Both stream progressively: `ready` flips after the first
        // staged chunk so audio starts without waiting for the full download.
        tracing::info!(target: "music::audio", %uri, has_fallback = fallback.is_some(), "stream staging: begin (primary source)");
        let mut wrote = download_into(&uri, &mut file, &ready, &fetched, &expected, &cancel);
        if wrote == 0
            && !cancel.load(Relaxed)
            && let Some(fb) = fallback
        {
            tracing::info!(target: "music::audio", fallback = %fb, "stream staging: primary yielded no media, trying fallback (direct)");
            // The primary wrote nothing, so the file restarts from zero and
            // the fallback's own declared length replaces the primary's —
            // otherwise a completeness check would compare the fallback's
            // bytes against the wrong total.
            fetched.store(0, Relaxed);
            expected.store(0, Relaxed);
            let _ = file.reset();
            wrote = download_into(&fb, &mut file, &ready, &fetched, &expected, &cancel);
        }
        let want = expected.load(Relaxed);
        if want > 0 && wrote < want {
            tracing::warn!(
                target: "music::audio",
                bytes = wrote,
                declared = want,
                "stream staging: source stopped short of the declared length — the staged file is a partial track"
            );
        }
        tracing::info!(target: "music::audio", bytes = wrote, declared = want, "stream staging: finished");
        let _ = std::io::Write::flush(&mut file);
        finish();
    });
}

/// Download `uri` into `file` using **bounded** `Range` requests (the only form
/// googlevideo serves; a proxy serves them over the whole file), flipping
/// `ready` after the first staged chunk so playback can start progressively.
/// Returns the number of bytes written.
#[cfg(all(feature = "audio", feature = "live"))]
#[allow(
    clippy::too_many_lines,
    reason = "range validation, bounded streaming, media sniffing, and cancellation must remain in one auditable transfer loop"
)]
fn download_into(
    uri: &str,
    file: &mut crate::files::Growing,
    ready: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    fetched: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    expected: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> u64 {
    const CHUNK: u64 = 1_048_576;

    use std::io::Read;
    use std::sync::atomic::Ordering::Relaxed;
    let Ok(client) = tilo_net::http::blocking_client()
        .user_agent(ua_for_uri(uri))
        // A blocking request cannot be interrupted by another thread. Keep its
        // deadline short so `cancel()` is observed between bounded requests,
        // rather than leaving a user waiting for a 30-second transfer timeout.
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return 0;
    };
    let mut clen = parse_clen(uri);
    expected.store(clen.unwrap_or(0), Relaxed);
    let mut off = 0u64;
    loop {
        if cancel.load(Relaxed) {
            break;
        }
        if let Some(c) = clen
            && off >= c
        {
            break;
        }
        let end = match clen {
            Some(c) => (off + CHUNK - 1).min(c - 1),
            None => off + CHUNK - 1,
        };
        let Some(mut resp) = request_range(&client, uri, off, end, cancel) else {
            tracing::debug!(target: "music::audio", %uri, off, "stream fetch: request failed");
            break;
        };
        if !resp.status().is_success() {
            tracing::debug!(target: "music::audio", %uri, off, status = %resp.status(), "stream fetch: non-success status");
            break;
        }
        let Some(want) = response_range_length(&resp, off, end, &mut clen) else {
            break;
        };
        expected.store(clen.unwrap_or(0), Relaxed);
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
        // Do not use `Response::bytes()`: a server ignoring Range could force
        // an unbounded allocation. Read at most this requested range plus one
        // sentinel byte, in a fixed buffer, and reject an oversized response.
        let mut remaining = want.saturating_add(1);
        let mut n = 0u64;
        let mut first = Vec::new();
        let mut buffer = vec![0u8; 64 * 1024].into_boxed_slice();
        while remaining > 0 {
            if cancel.load(Relaxed) {
                return 0;
            }
            let cap = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
            let Some(read_buffer) = buffer.get_mut(..cap) else {
                return 0;
            };
            let read = resp.read(read_buffer).unwrap_or(0);
            if read == 0 {
                break;
            }
            remaining = remaining.saturating_sub(read as u64);
            if first.len() < 1024 {
                let end = (1024 - first.len()).min(read);
                if let Some(chunk) = buffer.get(..end) {
                    first.extend_from_slice(chunk);
                } else {
                    return 0;
                }
            }
            if n.saturating_add(read as u64) > want {
                tracing::warn!(target: "music::audio", %uri, off, "stream fetch: source ignored bounded range");
                return 0;
            }
            if off == 0 && crate::backend::proxy::body_sniffs_as_html(&first) {
                tracing::warn!(target: "music::audio", %uri, "stream fetch: body sniffs as HTML — discarding, will try fallback");
                return 0;
            }
            let Some(chunk) = buffer.get(..read) else {
                return 0;
            };
            if std::io::Write::write_all(file, chunk).is_err() {
                return 0;
            }
            n = n.saturating_add(read as u64);
        }
        if n == 0 || remaining == 0 {
            // `remaining == 0` means the extra sentinel was consumed: reject
            // the response rather than accepting a full ignored-range body.
            return 0;
        }
        let _ = std::io::Write::flush(file);
        off += n;
        fetched.store(off, Relaxed);
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
///
/// A [`AudioOutput::Capture`] ends the same chain in an `appsink` instead,
/// fixed to signed 16-bit PCM so the probe can record the samples themselves.
/// It keeps `sync=true`, so playback position advances in real time exactly as
/// it does against a sound server. Each rendered sample is recorded and
/// released at once, and end-of-stream is not held back for a reader; either
/// would stall the pipeline where a real sink would not.
#[cfg(feature = "audio")]
fn build_audio_sink(
    playbin: &gstreamer::Element,
    frames: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    output: &AudioOutput,
) -> Option<gstreamer::Element> {
    use gstreamer::prelude::*;
    let (bin, sink) = build_audio_sink_bin(frames, output)?;
    playbin.set_property("audio-sink", &bin);
    Some(sink)
}

#[cfg(feature = "audio")]
fn build_audio_sink_bin(
    frames: &std::sync::Arc<std::sync::atomic::AtomicU64>,
    output: &AudioOutput,
) -> Option<(gstreamer::Bin, gstreamer::Element)> {
    use gstreamer::prelude::*;
    // `ghost_unlinked_pads=true` auto-exposes the audioconvert sink pad so
    // playbin can feed decoded audio into the chain (same idiom as the video
    // sink bin in `apps/tube`). `autoaudiosink` plugs the real system sink.
    let description = match output {
        AudioOutput::System => {
            "audioconvert ! audioresample ! autoaudiosink name=tilo_audiosink sync=true"
        }
        AudioOutput::Capture(_) => {
            "audioconvert ! audioresample ! appsink name=tilo_audiosink sync=true \
             caps=audio/x-raw,format=S16LE,layout=interleaved \
             emit-signals=true max-buffers=1 drop=true wait-on-eos=false"
        }
    };
    let bin = gstreamer::parse::bin_from_description(description, true).ok()?;
    let sink = bin.by_name("tilo_audiosink")?;
    // Count decoded PCM buffers arriving at the sink, and record a capture's.
    let capture = match output {
        AudioOutput::Capture(capture) => Some(capture.clone()),
        AudioOutput::System => None,
    };
    if let Some(pad) = sink.static_pad("sink") {
        let frames = frames.clone();
        let capture = capture.clone();
        pad.add_probe(gstreamer::PadProbeType::BUFFER, move |_, info| {
            frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(capture) = &capture
                && let Some(buffer) = info.buffer()
                && let Ok(map) = buffer.map_readable()
            {
                capture.record(map.as_slice());
            }
            gstreamer::PadProbeReturn::Ok
        });
    }
    // A buffer arrives while the pipeline is still prerolling; the capture
    // also counts what the sink renders once the clock says it is time to
    // play it, as a speaker would.
    if let Some(capture) = capture {
        sink.connect("new-sample", false, move |values| {
            let sample = values
                .first()
                .and_then(|value| value.get::<gstreamer::Element>().ok())
                .and_then(|sink| {
                    sink.emit_by_name::<Option<gstreamer::Sample>>("pull-sample", &[])
                });
            if sample.is_some() {
                capture.note_rendered();
            }
            Some(gstreamer::FlowReturn::Ok.to_value())
        });
    }
    Some((bin, sink))
}

#[cfg(feature = "audio")]
impl Drop for AudioBackend {
    fn drop(&mut self) {
        use gstreamer::prelude::*;
        if let Some(output) = &self.inner.governed_output {
            output.close();
        }
        if self.inner.controls.is_none() {
            let _ = self.inner.playbin.set_state(gstreamer::State::Null);
        }
        #[cfg(feature = "live")]
        if let Some(p) = self.inner.pending.take() {
            p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            crate::files::remove(&p.path);
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "audio")]
    fn isolated_governed_control_child() -> bool {
        const CHILD: &str = "TILO_TEST_MUSIC_GOVERNED_CONTROLS";
        if std::env::var_os(CHILD).is_none() {
            // The admitted output factory is process-global. Exercise production
            // controls in a fresh process without changing other tests' route.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "audio::tests::governed_controls_remain_responsive_and_fence_replaced_sources",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return true;
        }
        false
    }

    #[cfg(feature = "audio")]
    fn install_blocked_output() -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        use media::audio_output::{Factory, Observation, Output};
        use std::sync::{Arc, Mutex, mpsc};
        struct GateFactory {
            entered: mpsc::Sender<()>,
            release: Mutex<Option<mpsc::Receiver<()>>>,
        }
        struct GateOutput {
            entered: mpsc::Sender<()>,
            release: Option<mpsc::Receiver<()>>,
            frames: u64,
        }
        impl Factory for GateFactory {
            fn open(&self) -> Result<Box<dyn Output>, String> {
                Ok(Box::new(GateOutput {
                    entered: self.entered.clone(),
                    release: self.release.lock().unwrap().take(),
                    frames: 0,
                }))
            }
        }
        impl Output for GateOutput {
            fn write(&mut self, samples: &[i16]) -> Result<Observation, String> {
                if let Some(release) = self.release.take() {
                    self.entered.send(()).unwrap();
                    release
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .map_err(|e| e.to_string())?;
                }
                self.frames += samples.len() as u64 / 2;
                Ok(self.cached_observation())
            }
            fn observation(&mut self) -> Result<Observation, String> {
                Ok(self.cached_observation())
            }
            fn cached_observation(&self) -> Observation {
                Observation {
                    rendered_frames: self.frames,
                    queued_frames: 0,
                    ready: true,
                }
            }
            fn cached_error(&self) -> Option<String> {
                None
            }
            fn pause(&mut self, _: bool) -> Result<(), String> {
                Ok(())
            }
            fn flush(&mut self) -> Result<(), String> {
                Ok(())
            }
            fn volume(&mut self, _: u8) -> Result<(), String> {
                Ok(())
            }
            fn close(&mut self) {}
        }
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        media::audio_output::install(Arc::new(GateFactory {
            entered,
            release: Mutex::new(Some(released)),
        }));
        (entering, release)
    }

    #[test]
    #[cfg(feature = "audio")]
    #[serial_test::serial(audio_backend)]
    fn governed_controls_remain_responsive_and_fence_replaced_sources() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;
        if isolated_governed_control_child() {
            return;
        }
        let (entering, release) = install_blocked_output();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blocked-output.wav");
        std::fs::write(&path, sine_wav(2, 48_000)).unwrap();
        let mut backend = super::AudioBackend::try_new(&super::AudioOutput::System).unwrap();
        backend.load_with_fallback(url::Url::from_file_path(path).unwrap().as_str(), None);
        backend.play();
        entering
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("real decoder entered governed write");
        let backend = Arc::new(Mutex::new(backend));
        let query_backend = backend.clone();
        let (answered, answer) = mpsc::channel();
        let query = std::thread::spawn(move || {
            let mut backend = query_backend.lock().unwrap();
            backend.pause();
            answered
                .send((
                    backend.position_ms(),
                    backend.duration_ms(),
                    backend.try_seek_ms(1000),
                    backend.is_playing_confirmed(),
                ))
                .unwrap();
        });
        let observed = answer.recv_timeout(Duration::from_millis(250));
        release.send(()).unwrap();
        query.join().unwrap();
        let mut owner =
            Arc::try_unwrap(backend).unwrap_or_else(|_| panic!("query released backend"));
        let backend = owner.get_mut().unwrap();
        wait_for(|| {
            backend
                .inner
                .controls
                .as_ref()
                .unwrap()
                .seek_result()
                .is_some()
        });
        assert_eq!(
            backend.inner.controls.as_ref().unwrap().seek_result(),
            Some(Ok(()))
        );
        // A queued old seek must never apply after a replacement URI is installed.
        assert_eq!(backend.try_seek_ms(1000), Some(Ok(())));
        let replacement = dir.path().join("replacement.wav");
        std::fs::write(&replacement, sine_wav(4, 48_000)).unwrap();
        backend.load_with_fallback(
            url::Url::from_file_path(&replacement).unwrap().as_str(),
            None,
        );
        backend.pause();
        wait_for(|| {
            use gstreamer::prelude::*;
            backend.inner.controls.as_ref().unwrap().ready()
                && backend.inner.playbin.current_state() == gstreamer::State::Paused
                && backend.duration_ms() == Some(4000)
        });
        assert_eq!(
            backend.position_ms(),
            Some(0),
            "old seek cannot move the replacement"
        );
        assert_eq!(
            backend.inner.controls.as_ref().unwrap().seek_result(),
            None,
            "the replacement cannot inherit an old seek result"
        );
        // Dropping a paused backend must stop its worker and native pipeline.
        let pipeline = backend.inner.playbin.clone();
        drop(owner);
        wait_for(|| {
            use gstreamer::prelude::*;
            pipeline.current_state() == gstreamer::State::Null
        });
        assert!(
            matches!(observed, Ok((_, _, Some(Ok(())), false))),
            "pause, observations and seek submission must return promptly while PCM delivery is blocked: {observed:?}"
        );
    }

    use super::*;

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !condition() {
            assert!(ui::time::now() < deadline, "audio lifecycle did not settle");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    #[cfg(feature = "audio")]
    fn empty_app_does_not_initialize_gstreamer() {
        use ui::widget::Widget as _;
        const CHILD: &str = "TILO_TEST_EMPTY_MUSIC_NO_GST";
        if std::env::var_os(CHILD).is_some() {
            // A fresh process is essential: other audio tests initialize the
            // process-global GStreamer runtime, hiding an eager constructor.
            assert_eq!(unsafe { gstreamer::ffi::gst_is_initialized() }, 0);
            let mut app = crate::MusicApp::offline();
            let mut state = ui::State::new();
            app.settle(&mut ui::widget::Edits::lend(&mut state.doc), &state.root);
            app.sync(&state.doc, &state.root);
            app.settle(&mut ui::widget::Edits::lend(&mut state.doc), &state.root);
            let mut engine = AudioEngine::new();
            engine.set_volume(0.3);
            engine.pause();
            engine.stop();
            assert!(!engine.pump());
            assert_eq!(engine.frames_rendered(), 0);
            assert!(engine.position_ms().is_none());
            assert!(!engine.needs_poll());
            assert_eq!(unsafe { gstreamer::ffi::gst_is_initialized() }, 0);
            return;
        }
        let scratch = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "audio::tests::empty_app_does_not_initialize_gstreamer",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("GST_PLUGIN_SYSTEM_PATH_1_0", scratch.path())
            .env("GST_PLUGIN_PATH_1_0", scratch.path())
            .env("GST_REGISTRY_1_0", scratch.path().join("registry.bin"))
            .env("XDG_DATA_HOME", scratch.path())
            .env("XDG_CONFIG_HOME", scratch.path())
            .env("XDG_CACHE_HOME", scratch.path())
            .env_remove("TILO_MUSIC_VIEW")
            .current_dir(scratch.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        assert!(!scratch.path().join("registry.bin").exists());
    }

    #[test]
    #[cfg(feature = "audio")]
    #[serial_test::serial(audio_backend)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn real_backend_applies_deferred_state_renders_and_reports_failures() {
        // Phase `initialization_failure_reaches_transport_without_fake_playback`.
        {
            let (release, wait) = std::sync::mpsc::channel();
            let mut engine = AudioEngine::new();
            engine.initialization = Some(Initialization::spawn(move || {
                wait.recv().unwrap();
                Err("missing playback plugin".into())
            }));
            engine.load("file:///unavailable.wav");
            engine.play();
            assert!(!engine.pump());
            assert!(engine.needs_poll());
            assert!(!engine.is_playing_real());
            assert_eq!(engine.frames_rendered(), 0);
            release.send(()).unwrap();
            wait_for(|| engine.pump());
            assert_eq!(
                engine.take_error().as_deref(),
                Some("missing playback plugin")
            );
            assert!(engine.take_error().is_none());
            assert!(
                engine.pump(),
                "failure remains latched until stopped or reloaded"
            );
            engine.stop();
            assert!(!engine.needs_poll());
            assert!(!engine.pump());
        }

        // Phase `deferred_seek_timeout_reports_error_and_stops_polling`.
        {
            let mut engine = AudioEngine::new();
            engine.backend = Some(AudioBackend::try_new(&engine.output).unwrap());
            // A source that has not prerolled cannot accept a seek. Exercise the
            // production timeout with the actual native backend still in NULL.
            engine.current_uri = Some("file:///not-yet-prerolled.wav".into());
            engine.pending_seek = Some(1000);
            engine.seek_started = Some(
                ui::time::now()
                    .checked_sub(std::time::Duration::from_secs(6))
                    .unwrap(),
            );
            assert!(engine.pump());
            assert!(
                engine
                    .take_error()
                    .unwrap()
                    .contains("did not become seekable")
            );
            assert!(!engine.needs_poll());
            assert!(!engine.is_playing_real());
            engine.stop();
            assert!(!engine.pump());
        }

        // Phase `delayed_backend_applies_latest_source_volume_seek_and_pause`.
        {
            use gstreamer::prelude::*;
            let scratch = tempfile::tempdir().unwrap();
            let path = scratch.path().join("latest.wav");
            std::fs::write(&path, sine_wav(3, 44_100)).unwrap();
            let uri = media::source::path_to_file_uri(&path).unwrap();
            let (release, wait) = std::sync::mpsc::channel();
            let mut engine = AudioEngine::new();
            let output = engine.output.clone();
            engine.initialization = Some(Initialization::spawn(move || {
                wait.recv().unwrap();
                AudioBackend::try_new(&output)
            }));
            engine.load("file:///obsolete-must-never-open.wav");
            engine.play();
            engine.load(&uri);
            engine.set_volume(0.05);
            engine.seek_ms(1000);
            engine.play();
            engine.pause();
            assert!(!engine.pump());
            assert!(!engine.is_playing_real());
            assert!(engine.position_ms().is_none());
            assert!(
                engine.needs_poll(),
                "paused initialization still needs completion"
            );
            release.send(()).unwrap();
            wait_for(|| {
                assert!(!engine.pump(), "{:?}", engine.take_error());
                engine.backend.is_some()
                    && engine.pending_seek.is_none()
                    && engine.position_ms().is_some_and(|position| position >= 900)
            });
            let backend = engine.backend.as_ref().unwrap();
            assert_eq!(backend.current_uri.as_deref(), Some(uri.as_str()));
            assert!(
                (backend.inner.playbin.property::<f64>("volume") - f64::from(0.05f32)).abs()
                    < 0.000_01,
                "GStreamer preserves the requested volume within its normalization precision"
            );
            assert!(!engine.is_playing_real());
            engine.play();
            assert!(engine.wait_until_playing(tilo_test_env::hang_bound()));
            wait_for(|| {
                assert!(!engine.pump());
                engine.frames_rendered() > 0
            });
            let pcm = engine
                .capture()
                .unwrap()
                .wait_for_more(std::time::Duration::from_secs(5));
            assert!(pcm.peak > 0, "the decoded tone reached the sink: {pcm:?}");
            engine.stop();
        }

        // Phase `seek_while_playing_repositions_and_keeps_playing`.
        // **Seek-while-playing proof** (`--features audio`): a real decoded WAV is
        // playing, then a seek to ~50% must reposition the transport (position lands
        // near 50%) and playback must *continue* — not snap to the end / EOS, which
        // was the reported "can't seek while playing" bug. Uses a fully-seekable
        // local file so this exercises the real `seek_simple` path.
        //
        // Requires the decode plugins; the audio is captured, not played aloud.
        {
            assert!(gstreamer::init().is_ok(), "gstreamer initialises");
            let need = [
                "filesrc",
                "wavparse",
                "audioconvert",
                "audioresample",
                "appsink",
            ];
            assert!(
                need.iter()
                    .all(|name| gstreamer::ElementFactory::find(name).is_some()),
                "real seek proof requires decode/sink plugins {need:?}"
            );

            // ~6 s of audio so a 50% seek lands at a clear ~3 s offset.
            let path = std::env::temp_dir()
                .join(format!("tilo-music-seek-音 # %-{}.wav", std::process::id()));
            std::fs::write(&path, sine_wav(6, 44_100)).expect("write test wav");
            let uri = media::source::path_to_file_uri(&path).expect("native audio file URI");

            let mut engine = AudioEngine::new();
            let capture = engine.capture().expect("tests capture by default").clone();
            engine.set_volume(0.05);
            engine.load(&uri);
            engine.play();
            assert!(
                engine.wait_until_playing(tilo_test_env::hang_bound()),
                "pipeline reached PLAYING"
            );

            // Duration should be known now (the WAV header carries it); the seek
            // clamp reads it.
            let dur = {
                let mut d = None;
                for _ in 0..40 {
                    capture.wait_for_more(std::time::Duration::from_millis(25));
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
                capture.wait_for_more(std::time::Duration::from_millis(25));
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
                capture.wait_for_more(std::time::Duration::from_millis(25));
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn initializer_cancel_stop_and_stub_state_are_honest() {
        // Phase `canceled_initializer_disposes_result_without_joining_or_polling`.
        {
            struct Owned(std::sync::mpsc::Sender<()>);
            impl Drop for Owned {
                fn drop(&mut self) {
                    let _ = self.0.send(());
                }
            }
            for drop_job in [false, true] {
                let (release, wait) = std::sync::mpsc::channel();
                let (disposed, disposal) = std::sync::mpsc::channel();
                let mut job = Initialization::spawn(move || {
                    wait.recv().unwrap();
                    Ok(Owned(disposed))
                });
                job.cancel();
                assert!(!job.canceled_and_finished());
                if drop_job {
                    drop(job);
                }
                release.send(()).unwrap();
                disposal.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            }
        }

        // Phase `stopped_initialization_does_not_poll_or_start_parallel_replacement`.
        {
            let (release, wait) = std::sync::mpsc::channel();
            let mut engine = AudioEngine::new();
            engine.initialization = Some(Initialization::spawn(move || {
                wait.recv().unwrap();
                Err("canceled initializer".into())
            }));
            let old_finished = engine.initialization.as_ref().unwrap().finished.clone();
            engine.load("file:///first.wav");
            engine.stop();
            assert!(!engine.needs_poll());
            assert!(!engine.pump());
            engine.load("file:///replacement.wav");
            assert!(!engine.pump());
            assert!(std::sync::Arc::ptr_eq(
                &old_finished,
                &engine.initialization.as_ref().unwrap().finished
            ));
            engine.stop();
            release.send(()).unwrap();
            wait_for(|| old_finished.load(std::sync::atomic::Ordering::Acquire));
            assert!(!engine.pump());
            assert!(engine.current_uri().is_none());
            assert!(engine.take_error().is_none());
        }

        // Phase `stub_tracks_uri_and_volume`.
        {
            let mut e = AudioEngine::new();
            e.load("https://example.test/audio.m4a");
            e.set_volume(0.5);
            assert_eq!(e.current_uri(), Some("https://example.test/audio.m4a"));
            assert!((e.volume() - 0.5).abs() < 1e-6);
        }

        // Phase `is_live_matches_feature`.
        {
            assert_eq!(AudioEngine::is_live(), cfg!(feature = "audio"));
        }
    }

    #[test]
    #[cfg(all(feature = "audio", feature = "live"))]
    #[serial_test::serial(audio_backend)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn bounded_download_and_staged_pump_honour_cancel_range_and_native_uri() {
        // Phase `bounded_download_observes_cancel_before_connecting`.
        {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let address = listener.local_addr().expect("address");
            let path =
                std::env::temp_dir().join(format!("tilo-music-cancel-{}.part", std::process::id()));
            let mut file = crate::files::Growing::create(&path).expect("partial");
            let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let fetched = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let expected = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            assert_eq!(
                download_into(
                    &format!("http://{address}/never-contacted"),
                    &mut file,
                    &ready,
                    &fetched,
                    &expected,
                    &cancel,
                ),
                0
            );
            assert!(matches!(
                listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ));
            drop(file);
            std::fs::remove_file(path).expect("release download fixture");
        }

        // Phase `bounded_download_rejects_server_ignoring_range`.
        {
            use std::io::{Read, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
            let address = listener.local_addr().expect("address");
            let body = vec![0x55; 1_048_577];
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .expect("headers");
                // Rejection based on headers can close before this body lands.
                let _ = stream.write_all(&body);
            });
            let path = std::env::temp_dir()
                .join(format!("tilo-music-oversize-{}.part", std::process::id()));
            let mut file = crate::files::Growing::create(&path).expect("partial");
            let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let fetched = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let expected = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            assert_eq!(
                download_into(
                    &format!("http://{address}/media"),
                    &mut file,
                    &ready,
                    &fetched,
                    &expected,
                    &cancel,
                ),
                0
            );
            server.join().expect("server");
            assert!(std::fs::metadata(&path).expect("partial metadata").len() <= 1_048_576);
            drop(file);
            std::fs::remove_file(path).expect("release download fixture");
        }

        // Phase `staged_file_pump_uses_native_uri_and_renders_audio`.
        {
            use gstreamer::prelude::*;
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, AtomicU64};

            let path = std::env::temp_dir().join(format!(
                "tilo-music-staged-音 # %-{}.wav",
                std::process::id()
            ));
            let wav = sine_wav(1, 44_100);
            std::fs::write(&path, &wav).expect("staged audio bytes");
            let capture = PcmCapture::default();
            let mut engine = AudioBackend::try_new(&AudioOutput::Capture(capture.clone()))
                .expect("native backend");
            engine.set_volume(0.05);
            // The transfer has completed; drive the same pending-file transition
            // that the download worker exposes to the production pump.
            engine.inner.pending = Some(Pending {
                path: path.clone(),
                ready: Arc::new(AtomicBool::new(true)),
                done: Arc::new(AtomicBool::new(true)),
                cancel: Arc::new(AtomicBool::new(false)),
                fetched: Arc::new(AtomicU64::new(wav.len() as u64)),
                expected: Arc::new(AtomicU64::new(wav.len() as u64)),
                want_play: true,
                started: false,
            });
            assert!(!engine.pump(), "staged audio did not report an error");
            assert!(engine.wait_until_playing(tilo_test_env::hang_bound()));
            let uri = engine
                .inner
                .playbin
                .property::<Option<String>>("current-uri")
                .expect("playbin committed the staged URI after entering PLAYING");
            assert_eq!(url::Url::parse(&uri).unwrap().to_file_path().unwrap(), path);
            let deadline = tilo_test_env::hang_deadline();
            let mut pcm = capture.snapshot();
            while pcm.peak == 0 && ui::time::now() < deadline {
                pcm = capture.wait_for_more(std::time::Duration::from_millis(100));
            }
            assert!(
                engine.frames_rendered() > 0 && pcm.peak > 0,
                "staged PCM reached the sink: {pcm:?}"
            );
            engine.stop();
            engine.clear_pending();
            assert!(
                !path.exists(),
                "stopped staged audio releases its temporary file"
            );
        }
    }

    #[test]
    #[cfg(all(feature = "audio", feature = "live"))]
    #[serial_test::serial(audio_backend)]
    fn stopping_staged_playback_cancels_fetch_and_never_starts_fallback() {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering::Acquire;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_listener = listener.try_clone().unwrap();
        let body = sine_wav(20, 44_100);
        let total = body.len();
        let (received, request) = std::sync::mpsc::channel();
        let (release, response) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = server_listener.accept().unwrap();
            let mut bytes = [0u8; 2048];
            assert!(stream.read(&mut bytes).unwrap() > 0);
            received.send(()).unwrap();
            response.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            let _ = write!(
                stream,
                "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: 1048576\r\nContent-Range: bytes 0-1048575/{total}\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(&body[..1_048_576]);
        });
        let mut engine =
            AudioBackend::try_new(&AudioOutput::Capture(PcmCapture::default())).unwrap();
        engine.load_with_fallback(
            &format!("http://{address}/track?clen={total}"),
            Some(&format!("http://{address}/fallback?clen={total}")),
        );
        let done = engine.inner.pending.as_ref().unwrap().done.clone();
        request.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        engine.stop();
        release.send(()).unwrap();
        server.join().unwrap();
        wait_for(|| done.load(Acquire));
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "Stop must cancel subsequent ranges and fallback requests"
        );
        assert!(engine.current_uri.is_none());
    }

    #[test]
    #[cfg(all(feature = "audio", feature = "live"))]
    fn staging_learns_total_from_content_range_without_url_length() {
        use std::io::{Read, Write};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering::Acquire},
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = sine_wav(1, 44_100);
        let total = body.len() as u64;
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            assert!(stream.read(&mut request).unwrap() > 0);
            write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: {total}\r\nContent-Range: bytes 0-{}/{total}\r\nConnection: close\r\n\r\n", total-1).unwrap();
            stream.write_all(&body).unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let mut file = crate::files::Growing::create(&temp.path().join("stage.wav")).unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let fetched = Arc::new(AtomicU64::new(0));
        let expected = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        assert_eq!(
            download_into(
                &format!("http://{address}/track"),
                &mut file,
                &ready,
                &fetched,
                &expected,
                &cancel
            ),
            total
        );
        server.join().unwrap();
        assert_eq!(
            expected.load(Acquire),
            total,
            "completeness must use the actual HTTP range metadata"
        );
        assert_eq!(fetched.load(Acquire), total);
    }

    #[cfg(all(feature = "audio", feature = "live"))]
    fn download_responses(responses: Vec<Vec<u8>>) -> (u64, u64, u64) {
        use std::io::{Read, Write};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let observation = listener.try_clone().unwrap();
        let server = std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                assert!(stream.read(&mut request).unwrap() > 0);
                assert!(
                    String::from_utf8_lossy(&request).contains("Range: bytes=")
                        || String::from_utf8_lossy(&request).contains("range: bytes=")
                );
                let _ = stream.write_all(&response);
            }
        });
        let temp = tempfile::tempdir().unwrap();
        let mut file = crate::files::Growing::create(&temp.path().join("stage.wav")).unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let fetched = Arc::new(AtomicU64::new(0));
        let expected = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let wrote = download_into(
            &format!("http://{address}/track"),
            &mut file,
            &ready,
            &fetched,
            &expected,
            &cancel,
        );
        server.join().unwrap();
        observation.set_nonblocking(true).unwrap();
        assert!(
            matches!(observation.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "no extra requests beyond the permitted transient retry"
        );
        (wrote, fetched.load(Relaxed), expected.load(Relaxed))
    }

    #[test]
    #[cfg(all(feature = "audio", feature = "live"))]
    fn range_transfer_retries_transient_failure_and_rejects_denial_mismatch_and_truncation() {
        let body = sine_wav(1, 44_100);
        let total = body.len() as u64;
        let media = |range: String, bytes: &[u8]| {
            let mut response = format!("HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: {total}\r\nContent-Range: {range}\r\nConnection: close\r\n\r\n").into_bytes();
            response.extend_from_slice(bytes);
            response
        };
        let success = media(format!("bytes 0-{}/{total}", total - 1), &body);
        let failure =
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec();
        assert_eq!(
            download_responses(vec![failure, success]),
            (total, total, total)
        );
        for status in [401, 403, 429] {
            let denied = format!(
                "HTTP/1.1 {status} Denied\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .into_bytes();
            assert_eq!(download_responses(vec![denied]), (0, 0, 0));
        }
        let wrong = media(format!("bytes 1-{total}/{}", total + 1), &body);
        assert_eq!(download_responses(vec![wrong]), (0, 0, 0));
        let partial = media(format!("bytes 0-{}/{total}", total - 1), &body[..4096]);
        let (wrote, fetched, expected) = download_responses(vec![partial]);
        assert_eq!((wrote, fetched, expected), (4096, 4096, total));
        assert!(fetched < expected, "truncation cannot look complete");

        let body = sine_wav(20, 44_100);
        let size = body.len();
        let mut first = format!("HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: 1048576\r\nContent-Range: bytes 0-1048575/{size}\r\nConnection: close\r\n\r\n").into_bytes();
        first.extend_from_slice(&body[..1_048_576]);
        assert_eq!(
            download_responses(vec![first.clone(), first]),
            (1_048_576, 1_048_576, size as u64),
            "a repeated initial range must never be appended as later bytes"
        );
    }

    /// The same production staging/decoder journey runs with a generated WAV
    /// normally, or a permitted public media URL supplied explicitly by the
    /// operator. A failed live observation fails this test; it never skips.
    #[test]
    #[cfg(all(feature = "audio", feature = "live"))]
    #[serial_test::serial(audio_backend)]
    fn complete_http_track_decodes_to_eos_and_supports_pause_and_seek() {
        use std::io::{Read, Write};
        let source = std::env::var("TILO_MUSIC_PUBLIC_MEDIA_URL").ok();
        let mut server = None;
        let uri = source.unwrap_or_else(|| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            server = Some(std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 2048];
                assert!(stream.read(&mut request).unwrap() > 0);
                let body = sine_wav(2, 44_100);
                let size = body.len();
                write!(stream, "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: {size}\r\nContent-Range: bytes 0-{}/{size}\r\nConnection: close\r\n\r\n", size-1).unwrap();
                stream.write_all(&body).unwrap();
            }));
            format!("http://{address}/track")
        });
        let capture = PcmCapture::default();
        let mut engine = AudioBackend::try_new(&AudioOutput::Capture(capture.clone())).unwrap();
        engine.load_with_fallback(&uri, None);
        // Stage completely before decoding to prove full-file EOS, independent
        // of network timing. This is the ordinary production staged file path.
        wait_for(|| engine.staging_finished());
        assert!(
            engine.staging_complete(),
            "public media must arrive completely: {:?}",
            engine.staged_progress()
        );
        let progress = engine.staged_progress().unwrap();
        assert_eq!(Some(progress.0), progress.1);
        assert!(progress.0 > 0);
        engine.play();
        wait_for(|| {
            assert!(!engine.pump());
            capture.snapshot().peak > 0
                && capture.snapshot().rendered > 0
                && engine.is_playing_confirmed()
                && engine.duration_ms().is_some()
                && engine.position_ms().is_some()
        });
        engine.pause();
        wait_for(|| {
            assert!(!engine.pump());
            !engine.is_playing_real() && engine.position_ms().is_some()
        });
        let paused = engine.position_ms().unwrap();
        capture.wait_for_more(std::time::Duration::from_millis(200));
        assert!(engine.position_ms().unwrap().abs_diff(paused) < 100);
        let duration = engine.duration_ms().unwrap();
        engine.play();
        let deadline = ui::time::now() + std::time::Duration::from_millis(duration + 30_000);
        while !engine.reached_eos() && ui::time::now() < deadline {
            assert!(!engine.pump());
            capture.wait_for_more(std::time::Duration::from_millis(100));
        }
        let pcm = capture.snapshot();
        assert!(engine.reached_eos(), "whole track must reach EOS: {pcm:?}");
        assert!(
            pcm.samples > duration * 40,
            "decoded the whole duration: {pcm:?}"
        );
        assert_eq!(
            engine.try_seek_ms(duration.saturating_sub(1500)),
            Some(Ok(()))
        );
        engine.play();
        wait_for(|| {
            assert!(!engine.pump());
            capture.snapshot().samples > pcm.samples
        });
        engine.stop();
        assert!(engine.inner.pending.is_none());
        if let Some(server) = server {
            server.join().unwrap();
        }
        eprintln!(
            "complete media: bytes={progress:?}, duration_ms={duration}, pcm={pcm:?}; pause, resume, seek and stop passed"
        );
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
    /// Requires WAV decode plugins and a real platform audio endpoint, so it
    /// runs where `TILO_REQUIRE_AUDIO=1` says one exists (`tools/with-headless-audio.sh`
    /// sets it). There, missing prerequisites fail this proof; a fake sink
    /// cannot establish playback. Every other test captures the decoded PCM
    /// through an `appsink` and needs no sound server.
    #[cfg(feature = "audio")]
    #[test]
    #[serial_test::serial(audio_backend)]
    fn real_sink_renders_audio_and_is_not_fakesink() {
        if std::env::var_os("TILO_REQUIRE_AUDIO").is_none_or(|value| value != "1") {
            eprintln!(
                "real audio output not checked: set TILO_REQUIRE_AUDIO=1 where a sound server runs"
            );
            return;
        }
        assert!(gstreamer::init().is_ok(), "gstreamer initialises");
        // The decode graph playbin needs for a local WAV, plus the real sink.
        let need = [
            "filesrc",
            "wavparse",
            "audioconvert",
            "audioresample",
            "autoaudiosink",
        ];
        assert!(
            need.iter()
                .all(|name| gstreamer::ElementFactory::find(name).is_some()),
            "real audio proof requires decode/sink plugins {need:?}"
        );

        // Write ~1s of quiet 16-bit-mono PCM sine (44.1 kHz) as a real WAV.
        let path = std::env::temp_dir().join(format!(
            "tilo-music-audio-音 # %-{}.wav",
            std::process::id()
        ));
        std::fs::write(&path, sine_wav(1, 44_100)).expect("write test wav");
        let uri = media::source::path_to_file_uri(&path).expect("native audio file URI");

        let mut engine = AudioEngine::with_output(AudioOutput::System);
        engine.set_volume(0.05); // audible path, but keep the test quiet
        engine.load(&uri);
        engine.play();

        let reached = engine.wait_until_playing(tilo_test_env::hang_bound());
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
