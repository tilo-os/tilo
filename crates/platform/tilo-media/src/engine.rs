//! The playback engine: one control surface, two interchangeable backends.
//!
//! * **`GStreamer` decode** (`--features decode`) drives the pipeline in
//!   `crate::gst_backend`: `playbin3` with hardware decoders ranked above
//!   software ones, a sink that negotiates the best memory the consumer
//!   declared it can take (dmabuf → planar → RGBA), frame-exact `ACCURATE`
//!   seeking that reads the preroll without ever starting the audio sink, and
//!   clock-driven playback. It decodes any codec `GStreamer` can plug from
//!   a local file, the FUSE workspace, or a stream URL.
//! * **Synthesized fallback** (default) produces a deterministic RGBA test
//!   pattern driven by a playhead clock, plus buffering progress
//!   from the prefetcher when a file source is opened. It needs no gstreamer
//!   stack, so hermetic builds, `--shots`, and CI do not require it.
//!
//! The public surface ([`MediaEngine`]) is identical for both backends. A
//! consumer declares what memory it can paint through [`EngineConfig`] and
//! [`FrameAcceptance`].

use std::sync::OnceLock;
use tilo_clock::Instant;

use crate::event::{MediaEvent, VideoFrame};
use crate::frame::{Frame, FrameAcceptance, FramePayload};
use crate::hw::{HwPolicy, HwReport};
use crate::ring::{PrefetchProgress, Prefetcher};
use crate::source::Source;
use crate::transport::{Correction, TransportState, correction};

/// A selectable elementary stream reported by the active decoder. The label is
/// intentionally conservative: if a container did not provide a human name,
/// callers receive "Audio track 1" rather than a guessed language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaTrack {
    /// Decoder supplied elementary-stream identity.  Unlike the selection
    /// index this remains meaningful when a container exposes its streams in
    /// a different order after a re-open.
    pub id: String,
    pub index: u32,
    pub label: String,
    pub selected: bool,
}

/// A chapter supplied by a container table of contents. `id` is the container
/// UID and `start_ms` is suitable for direct, frame-accurate seeking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaChapter {
    pub id: String,
    pub label: String,
    pub start_ms: u64,
}

#[cfg(gst_decode)]
pub use crate::gst_backend::SeekMode;

/// Which seek precision the engine uses when nothing more specific is asked
/// for. Exact by default: an editor scrubbing to 12.500s must see the frame at
/// 12.500s, not the keyframe up to two seconds behind it.
#[cfg(not(gst_decode))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekMode {
    /// Present the frame at the requested time.
    Accurate,
    /// Snap to the nearest keyframe — cheap, coarse.
    Keyframe,
}

/// How an engine should be built.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    /// What memory the consumer can paint. [`FrameAcceptance::Rgba`] (the
    /// default) keeps the long-standing [`VideoFrame`] behaviour;
    /// [`FrameAcceptance::Native`] lets the pipeline keep pictures on the GPU.
    pub acceptance: FrameAcceptance,
    /// Hardware-decode policy. Defaults to whatever `TILO_MEDIA_HWDEC` says.
    pub hw_policy: HwPolicy,
    /// Starting output volume, `0.0..=1.0`.
    pub volume: f32,
    /// Whether the engine plays sound at all. Without it no audio stream is
    /// decoded and no audio device is opened.
    pub audio_output: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            acceptance: FrameAcceptance::Rgba,
            hw_policy: HwPolicy::from_env(),
            volume: 0.8,
            audio_output: true,
        }
    }
}

impl EngineConfig {
    /// The configuration a GPU-side consumer wants: frames stay in whatever
    /// memory the decoder produced them in, ready for a zero-copy import.
    pub fn gpu() -> Self {
        Self {
            acceptance: FrameAcceptance::Native,
            ..Self::default()
        }
    }

    /// Start muted — the editor's preview and every thumbnail decode, which
    /// must never make a sound.
    #[must_use]
    pub fn silent(mut self) -> Self {
        self.volume = 0.0;
        self
    }

    /// Decode pictures only: thumbnails, export and the editor's preview,
    /// which never make a sound. No audio stream is decoded and no audio
    /// device is opened, so a machine without one, or an audio server that
    /// is busy or gone, never stops a picture.
    #[must_use]
    pub fn frames_only(mut self) -> Self {
        self.volume = 0.0;
        self.audio_output = false;
        self
    }

    /// Decode in software regardless of what hardware is present. The escape
    /// hatch for a driver that misbehaves, and what the fallback path is
    /// tested through.
    #[must_use]
    pub fn software_only(mut self) -> Self {
        self.hw_policy = HwPolicy::Off;
        self
    }

    /// Force one decoder family to the top of autoplugging.
    #[must_use]
    pub fn with_hw_policy(mut self, policy: HwPolicy) -> Self {
        self.hw_policy = policy;
        self
    }
}

/// Placeholder stream length for the synthesized fallback when the duration is
/// unknown. The decode backend overrides this with the pipeline's duration.
const DEFAULT_SYNTH_DURATION_MS: u64 = 120_000;

/// Synthesized-frame dimensions. 16:9, small enough to regenerate cheaply each
/// frame, large enough to read as "a video" in a `--shots` capture.
const SYNTH_W: u32 = 480;
const SYNTH_H: u32 = 270;
/// The same dimensions as byte counts. Derived once, at compile time, so the
/// render loop below carries no size arithmetic that could be got wrong.
const SYNTH_ROW_BYTES: usize = SYNTH_W as usize * 4;
const SYNTH_RGBA_BYTES: usize = SYNTH_ROW_BYTES * SYNTH_H as usize;

/// Process-monotonic millisecond clock for the fallback playhead. A lazily
/// pinned `Instant` base keeps it cheap and immune to wall-clock jumps.
fn mono_now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    // `as_millis` is `u128`. Saturating rather than truncating: a process that
    // has been up for 584 million years should report "a very long time", not
    // wrap its playhead back to zero.
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Why [`MediaEngine::seek_and_wait`] returned without a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeekWaitError {
    /// The pipeline reported a fatal error.
    Failed(String),
    /// No picture landed before the timeout.
    TimedOut,
}

impl std::fmt::Display for SeekWaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(reason) => write!(f, "the pipeline failed: {reason}"),
            Self::TimedOut => f.write_str("no picture arrived in time"),
        }
    }
}

impl std::error::Error for SeekWaitError {}

/// A collaborative, source-agnostic media engine. Construct with [`Self::new`],
/// point it at a [`Source`] with [`Self::open`], drive it with `play` / `pause`
/// / `seek_ms` / `set_volume`, and pump it once per frame with [`Self::poll`],
/// which returns the [`MediaEvent`]s since the last call and refreshes
/// [`Self::latest_frame`].
pub struct MediaEngine {
    handoff_id: u64,
    handoff_controller_bus_name: Option<String>,
    backend: Backend,
    source: Option<Source>,
    volume: f32,
    muted: bool,
    /// The requested playback multiplier. Kept at the engine boundary so the
    /// synthesized fallback, decoder, collaborative transport, and every
    /// consumer agree on the one value the user selected.
    rate: f32,
    config: EngineConfig,
}

enum Backend {
    Synth(SynthState),
    /// Boxed: the pipeline state is far larger than the other backends.
    #[cfg(gst_decode)]
    Gst(Box<crate::gst_backend::GstState>),
    /// The decode backend is built in but its pipeline could not be made.
    #[cfg(gst_decode)]
    Unavailable(UnavailableState),
    #[cfg(web_page)]
    Web(crate::web::WebState),
}

impl MediaEngine {
    fn next_handoff_id() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .max(1)
    }

    fn refresh_handoff(&self) {
        crate::handoff::update(
            self.handoff_id,
            crate::handoff::EnginePlayback {
                source: self.source.as_ref(),
                audible: self.volume > 0.0,
                position_ms: self.position_ms(),
                duration_ms: self.duration_ms(),
                playing: self.is_playing(),
                has_video: self.latest().is_some(),
                controller_bus_name: self.handoff_controller_bus_name.as_deref(),
            },
        );
    }

    /// Bind this engine to the exact MPRIS player that can pause it after a
    /// destination acknowledges a cross-device handoff.
    pub fn set_handoff_controller(&mut self, bus_name: Option<String>) {
        self.handoff_controller_bus_name = bus_name;
        self.refresh_handoff();
    }

    /// Start the decode backend's one-time process set-up in the background,
    /// so the first engine built later finds it done: an app calls this when
    /// it learns it will play something soon (a person opened a video whose
    /// stream is still being resolved), and the set-up overlaps that wait.
    /// A no-op without the decode backend and after the first call.
    pub fn prepare_in_background() {
        #[cfg(gst_decode)]
        crate::gst_backend::prepare_in_background();
    }

    /// Build an engine with the default configuration: system-memory RGBA
    /// frames, hardware decode per `TILO_MEDIA_HWDEC`. With `--features decode`
    /// this stands up a `GStreamer` pipeline. Without the feature it is the
    /// synthesized backend. With it, a pipeline that cannot be built is never
    /// replaced by synthesized pictures: the engine reports
    /// [`Self::is_real_decode`] `false` and every open fails with
    /// [`MediaEvent::Error`] saying why. Never panics.
    pub fn new() -> Self {
        Self::with_config(EngineConfig::default())
    }

    /// Build an engine from an explicit configuration.
    pub fn with_config(config: EngineConfig) -> Self {
        #[cfg(web_page)]
        {
            if let Some(web) = crate::web::WebState::new() {
                return Self {
                    handoff_id: Self::next_handoff_id(),
                    handoff_controller_bus_name: None,
                    backend: Backend::Web(web),
                    source: None,
                    volume: config.volume,
                    muted: false,
                    rate: 1.0,
                    config,
                };
            }
        }
        #[cfg(gst_decode)]
        let backend = match crate::gst_backend::GstState::new(
            config.acceptance,
            config.hw_policy,
            config.audio_output,
        ) {
            Ok(gst) => Backend::Gst(Box::new(gst)),
            Err(reason) => {
                tracing::error!(%reason, "media: no decode pipeline");
                Backend::Unavailable(UnavailableState {
                    reason,
                    pending: false,
                })
            }
        };
        #[cfg(not(gst_decode))]
        let backend = Backend::Synth(SynthState::new());
        Self {
            handoff_id: Self::next_handoff_id(),
            handoff_controller_bus_name: None,
            backend,
            source: None,
            volume: config.volume,
            muted: false,
            rate: 1.0,
            config,
        }
    }

    /// Have the synthesized backend answer its next seek with a picture
    /// stamped `pts_ms`, as a pipeline still running when the seek arrived
    /// can hand over the frame it decoded for where it was.
    #[cfg(test)]
    pub(crate) fn offer_stale_on_next_seek(&mut self, pts_ms: u64) {
        match &mut self.backend {
            Backend::Synth(s) => s.stale_on_seek = Some(pts_ms),
            #[cfg(gst_decode)]
            Backend::Gst(_) | Backend::Unavailable(_) => {}
            #[cfg(web_page)]
            Backend::Web(_) => {}
        }
    }

    /// Force the synthesized backend regardless of build features. Used by the
    /// hermetic engine tests so event ordering and transport application are
    /// exercised deterministically without the decode backend.
    pub fn new_synthesized() -> Self {
        Self {
            handoff_id: Self::next_handoff_id(),
            handoff_controller_bus_name: None,
            backend: Backend::Synth(SynthState::new()),
            source: None,
            volume: 0.8,
            muted: false,
            rate: 1.0,
            config: EngineConfig::default(),
        }
    }

    /// The configuration this engine was built with.
    pub fn config(&self) -> EngineConfig {
        self.config
    }

    /// What the decode pipeline plugged: the decoder element's name,
    /// its family, and the caps the sink negotiated.
    ///
    /// This is an observation of the pipeline rather than a restatement of
    /// configuration. Until a frame has prerolled it reports nothing, and
    /// [`HwReport::is_hardware`] is `false`.
    pub fn hw_report(&self) -> HwReport {
        match &self.backend {
            Backend::Synth(_) => HwReport::default(),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.hw_report(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => HwReport::default(),
            #[cfg(web_page)]
            Backend::Web(_) => HwReport::default(),
        }
    }

    /// Whether this engine is decoding through a `GStreamer` pipeline (vs.
    /// the synthesized fallback).
    pub fn is_real_decode(&self) -> bool {
        match &self.backend {
            Backend::Synth(_) => false,
            #[cfg(gst_decode)]
            Backend::Gst(_) => true,
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => false,
            #[cfg(web_page)]
            Backend::Web(_) => true,
        }
    }

    /// Whether the decode backend is compiled into this build.
    pub fn is_decode_compiled() -> bool {
        cfg!(any(gst_decode, web_page))
    }

    /// The source currently loaded, if any.
    pub fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

    /// Point the engine at `source` and reset to the start, paused. The stream
    /// duration surfaces on the next [`Self::poll`] as
    /// [`MediaEvent::DurationKnown`].
    pub fn open(&mut self, source: Source) {
        match &mut self.backend {
            Backend::Synth(s) => s.open(&source, self.volume),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.open(&source, self.volume),
            #[cfg(gst_decode)]
            Backend::Unavailable(u) => u.open(),
            #[cfg(web_page)]
            Backend::Web(w) => w.open(&source, self.volume),
        }
        // Handed over rather than cloned: the backend only borrows it, and no
        // backend reads `self.source`, so recording it afterwards is the same
        // observable state without the copy.
        self.source = Some(source);
        self.refresh_handoff();
    }

    /// Start (or resume) playback.
    pub fn play(&mut self) {
        match &mut self.backend {
            Backend::Synth(s) => s.set_playing(true),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.set_playing(true),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => {}
            #[cfg(web_page)]
            Backend::Web(w) => w.set_playing(true),
        }
        self.refresh_handoff();
    }

    /// Pause playback, holding the playhead in place.
    pub fn pause(&mut self) {
        match &mut self.backend {
            Backend::Synth(s) => s.set_playing(false),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.set_playing(false),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => {}
            #[cfg(web_page)]
            Backend::Web(w) => w.set_playing(false),
        }
        self.refresh_handoff();
    }

    /// Toggle play/pause.
    pub fn toggle(&mut self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Whether playback is currently running.
    pub fn is_playing(&self) -> bool {
        match &self.backend {
            Backend::Synth(s) => s.playing,
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.is_playing(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => false,
            #[cfg(web_page)]
            Backend::Web(w) => w.is_playing(),
        }
    }

    /// Seek to an absolute playhead position in milliseconds.
    ///
    /// **Frame-exact.** The decoder runs forward from the preceding keyframe
    /// so the picture presented is the one at `ms`, not the keyframe before
    /// it — which with a typical two-second GOP could be nearly two seconds
    /// away, and left non-keyframes unreachable entirely.
    ///
    /// When the pipeline is paused (the scrubbing case) the new picture
    /// arrives on the preroll path and the audio sink is never started, so
    /// scrubbing is silent.
    pub fn seek_ms(&mut self, ms: u64) {
        let _ = self.seek_with(ms, SeekMode::Accurate);
    }

    /// Seek at an explicit precision. [`SeekMode::Keyframe`] is the cheap,
    /// coarse option for a fast drag where landing within a GOP is acceptable;
    /// [`SeekMode::Accurate`] is the default everywhere else.
    pub fn seek_with(&mut self, ms: u64, mode: SeekMode) -> bool {
        let accepted = match &mut self.backend {
            Backend::Synth(s) => {
                let _ = mode;
                s.seek(ms);
                true
            }
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.seek(ms, mode),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => false,
            #[cfg(web_page)]
            Backend::Web(w) => {
                let _ = mode;
                w.seek(ms);
                true
            }
        };
        self.refresh_handoff();
        accepted
    }

    /// Seek to `ms` and pump until the picture for that time has landed, the
    /// pipeline fails, or `timeout` elapses.
    ///
    /// The blocking counterpart to [`Self::seek_ms`], for callers that need a
    /// specific frame in hand rather than eventually — extracting a thumbnail,
    /// or settling a scrub before a screenshot. An interactive preview should
    /// keep using `seek_ms` + [`Self::poll`] and stay non-blocking.
    ///
    /// # Errors
    ///
    /// [`SeekWaitError::Failed`] with the pipeline's reason, or
    /// [`SeekWaitError::TimedOut`] when no picture landed in `timeout`.
    pub fn seek_and_wait(
        &mut self,
        ms: u64,
        timeout: tilo_clock::Duration,
    ) -> Result<(), SeekWaitError> {
        // `Instant + Duration` panics on overflow, and the only way to reach
        // that is a `timeout` of hundreds of billions of years — a caller
        // spelling "wait as long as it takes" as `Duration::MAX`. `None` here
        // is that request honoured: pump until the frame lands. Aborting the
        // process instead would be the one answer nobody asked for.
        // The page cannot block: the element decodes on the event loop this
        // call would be holding up. One attempt, then report what landed.
        #[cfg(web_page)]
        if matches!(self.backend, Backend::Web(_)) {
            let _ = timeout;
            let accepted = self.seek_with(ms, SeekMode::Accurate);
            if let Some(reason) = Self::failure(self.poll()) {
                return Err(SeekWaitError::Failed(reason));
            }
            return if accepted && self.frame_is_current() {
                Ok(())
            } else {
                Err(SeekWaitError::TimedOut)
            };
        }
        #[cfg(gst_decode)]
        if let Backend::Unavailable(u) = &self.backend {
            return Err(SeekWaitError::Failed(u.reason.clone()));
        }
        let deadline = Instant::now().checked_add(timeout);
        // A pipeline that has not finished prerolling refuses seeks, so the
        // request has to be repeated until it is taken. Waiting on a seek that
        // was never accepted is the difference between "the frame is coming"
        // and hanging for the whole timeout.
        let mut accepted = self.seek_with(ms, SeekMode::Accurate);
        loop {
            #[cfg(gst_decode)]
            if let Backend::Gst(g) = &mut self.backend {
                // A flushing seek is asynchronous: the new preroll exists only
                // once the state change completes.
                g.wait_settled(tilo_clock::Duration::from_millis(50));
            }
            if !accepted {
                accepted = self.seek_with(ms, SeekMode::Accurate);
            }
            // A pipeline error stops the engine: no picture is coming.
            if let Some(reason) = Self::failure(self.poll()) {
                return Err(SeekWaitError::Failed(reason));
            }
            if accepted && self.frame_is_current() {
                return Ok(());
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(SeekWaitError::TimedOut);
            }
            std::thread::sleep(tilo_clock::Duration::from_millis(5));
        }
    }

    fn failure(events: Vec<MediaEvent>) -> Option<String> {
        events.into_iter().find_map(|event| match event {
            MediaEvent::Error(reason) => Some(reason),
            _ => None,
        })
    }

    /// Set output volume in `0.0..=1.0`.
    ///
    /// Applied to a dedicated gain element in the audio chain rather than to
    /// `playbin`'s own property, so changing it does not re-negotiate the
    /// stream. Muting is separate ([`Self::set_muted`]) and preserves this
    /// level.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        match &mut self.backend {
            Backend::Synth(_) => {}
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.set_volume(self.volume),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => {}
            #[cfg(web_page)]
            Backend::Web(w) => w.set_volume(self.volume),
        }
        self.refresh_handoff();
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Silence the output without losing the volume setting.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        match &mut self.backend {
            Backend::Synth(_) => {}
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.set_muted(muted),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => {}
            #[cfg(web_page)]
            Backend::Web(w) => w.set_muted(muted),
        }
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Set playback speed. Values outside the deliberately small, intelligible
    /// range are clamped rather than passed to a backend that may interpret a
    /// negative or denormal rate differently. `1.0` is normal speed.
    pub fn set_rate(&mut self, rate: f32) {
        self.rate = rate.clamp(0.25, 4.0);
        match &mut self.backend {
            Backend::Synth(state) => state.set_rate(self.rate),
            #[cfg(gst_decode)]
            Backend::Gst(state) => state.set_rate(self.rate),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => {}
            #[cfg(web_page)]
            Backend::Web(state) => state.set_rate(self.rate),
        }
    }

    /// The effective playback speed, always in `0.25..=4.0`.
    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// Enumerate audio tracks after the container has exposed them. An empty
    /// list is expected for the synthesized backend and for video-only media.
    pub fn audio_tracks(&self) -> Vec<MediaTrack> {
        match &self.backend {
            Backend::Synth(_) => Vec::new(),
            #[cfg(gst_decode)]
            Backend::Gst(state) => state.audio_tracks(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => Vec::new(),
            #[cfg(web_page)]
            Backend::Web(_) => Vec::new(),
        }
    }

    /// Enumerate subtitle/text tracks exposed by the active container.
    pub fn subtitle_tracks(&self) -> Vec<MediaTrack> {
        match &self.backend {
            Backend::Synth(_) => Vec::new(),
            #[cfg(gst_decode)]
            Backend::Gst(state) => state.subtitle_tracks(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => Vec::new(),
            #[cfg(web_page)]
            Backend::Web(_) => Vec::new(),
        }
    }

    /// Container table-of-contents chapters observed by the decoder. An empty
    /// list means the container has not supplied chapter metadata.
    pub fn chapters(&self) -> Vec<MediaChapter> {
        match &self.backend {
            Backend::Synth(_) => Vec::new(),
            #[cfg(gst_decode)]
            Backend::Gst(state) => state.chapters(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => Vec::new(),
            #[cfg(web_page)]
            Backend::Web(_) => Vec::new(),
        }
    }

    /// Select an audio stream by its stable, decoder-provided index.
    pub fn select_audio_track(&mut self, index: u32) -> bool {
        match &mut self.backend {
            Backend::Synth(_) => {
                let _ = index;
                false
            }
            #[cfg(gst_decode)]
            Backend::Gst(state) => state.select_audio_track(index),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => false,
            #[cfg(web_page)]
            Backend::Web(_) => false,
        }
    }

    /// Select a subtitle stream, or `None` to turn subtitles off.
    pub fn select_subtitle_track(&mut self, index: Option<u32>) -> bool {
        match &mut self.backend {
            Backend::Synth(_) => {
                let _ = index;
                false
            }
            #[cfg(gst_decode)]
            Backend::Gst(state) => state.select_subtitle_track(index),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => false,
            #[cfg(web_page)]
            Backend::Web(_) => false,
        }
    }

    /// The audio sink element, for routing or monitoring this source's output
    /// — setting a `PipeWire` target node, attaching a level meter, or tapping a
    /// tee into a mixer. `None` on the synthesized backend (no audio at all)
    /// and when the audio chain could not be built.
    ///
    /// Mixing several sources together is the caller's job; a mixer attaches
    /// to one source's output here.
    #[cfg(gst_decode)]
    pub fn audio_sink(&self) -> Option<gstreamer::Element> {
        match &self.backend {
            Backend::Synth(_) | Backend::Unavailable(_) => None,
            Backend::Gst(g) => g.audio_sink(),
        }
    }

    /// The current playhead in milliseconds.
    pub fn position_ms(&self) -> u64 {
        match &self.backend {
            Backend::Synth(s) => s.position,
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.position_ms(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => 0,
            #[cfg(web_page)]
            Backend::Web(w) => w.position_ms(),
        }
    }

    /// The total duration in milliseconds, once known.
    pub fn duration_ms(&self) -> Option<u64> {
        match &self.backend {
            Backend::Synth(s) => Some(s.duration),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.duration_ms(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => None,
            #[cfg(web_page)]
            Backend::Web(w) => w.duration_ms(),
        }
    }

    /// Buffering / prefetch progress, 0..=100.
    pub fn buffering_percent(&self) -> u8 {
        match &self.backend {
            Backend::Synth(s) => s.buffering_percent(),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.buffering_percent(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => 100,
            #[cfg(web_page)]
            Backend::Web(w) => w.buffering_percent(),
        }
    }

    /// The most recently produced frame as system-memory RGBA, if any.
    ///
    /// Always populated under the default [`FrameAcceptance::Rgba`] config.
    /// Under [`FrameAcceptance::Native`] a picture may be a dmabuf or planar
    /// YUV, in which case this is `None` — reading it must not trigger a
    /// hidden GPU readback. Those consumers use [`Self::latest`].
    pub fn latest_frame(&self) -> Option<&VideoFrame> {
        match &self.backend {
            Backend::Synth(s) => s.latest.as_ref(),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.latest_frame(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => None,
            #[cfg(web_page)]
            Backend::Web(w) => w.latest_frame(),
        }
    }

    /// Have the pictures stay where the page's media element shows them
    /// (web): [`Self::take_element_picture`] hands them out and the engine
    /// reads no pixels back for [`Self::latest_frame`]. A no-op elsewhere.
    pub fn prefer_element_pictures(&mut self) {
        #[cfg(web_page)]
        if let Backend::Web(w) = &mut self.backend {
            w.prefer_element_pictures();
        }
    }

    /// The newest picture as it stays in the page's media element, once per
    /// picture, after [`Self::prefer_element_pictures`]; `None` while nothing
    /// new shows, and always off the web.
    pub fn take_element_picture(&mut self) -> Option<crate::ElementPicture> {
        match &mut self.backend {
            #[cfg(web_page)]
            Backend::Web(w) => w.take_element_picture(),
            _ => None,
        }
    }

    /// Whether the held picture belongs to the most recent seek, rather than
    /// to the position before it.
    ///
    /// A scrub keeps painting the previous frame while the next one decodes,
    /// so "a frame exists" is not the same question as "the frame for where
    /// the playhead is now has arrived". A preview uses this to know whether
    /// what it is showing is current.
    pub fn frame_is_current(&self) -> bool {
        match &self.backend {
            Backend::Synth(s) => s.latest.is_some(),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.latest().is_some() && g.frame_is_post_seek(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => false,
            #[cfg(web_page)]
            Backend::Web(w) => w.frame_is_current(),
        }
    }

    /// The most recent picture in whatever memory the decoder produced it in.
    ///
    /// A [`FramePayload::Dmabuf`] is still on the GPU and ready for a
    /// zero-copy import; a [`FramePayload::Planar`] is NV12/P010 in system
    /// memory, ready for a two-plane upload; a [`FramePayload::Rgba`] is the
    /// packed buffer [`Self::latest_frame`] also exposes.
    pub fn latest(&self) -> Option<&Frame> {
        match &self.backend {
            Backend::Synth(s) => s.latest_native.as_ref(),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.latest(),
            #[cfg(gst_decode)]
            Backend::Unavailable(_) => None,
            #[cfg(web_page)]
            Backend::Web(w) => w.latest(),
        }
    }

    /// Pump the engine: pull any new frame, drain backend events, advance the
    /// playhead. Returns the [`MediaEvent`]s produced since the last call.
    /// Call once per rendered frame.
    pub fn poll(&mut self) -> Vec<MediaEvent> {
        let events = match &mut self.backend {
            Backend::Synth(s) => s.poll_at(mono_now_ms()),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.poll(),
            #[cfg(gst_decode)]
            Backend::Unavailable(u) => u.poll(),
            #[cfg(web_page)]
            Backend::Web(w) => w.poll(),
        };
        self.refresh_handoff();
        events
    }

    /// Test seam: pump the synthesized backend at an explicit virtual `now_ms`
    /// so event ordering is deterministic without wall-clock time. On a decode
    /// backend this behaves like [`Self::poll`] (the argument is ignored).
    pub fn poll_at(&mut self, now_ms: u64) -> Vec<MediaEvent> {
        let events = match &mut self.backend {
            Backend::Synth(s) => s.poll_at(now_ms),
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.poll(),
            #[cfg(gst_decode)]
            Backend::Unavailable(u) => u.poll(),
            #[cfg(web_page)]
            Backend::Web(w) => w.poll(),
        };
        self.refresh_handoff();
        events
    }

    /// Describe what is playing to the system's now-playing controls. On the
    /// web this is the page's Media Session; on Linux the toolkit's MPRIS
    /// service publishes the `VideoPlayer` widget's engine, so this is a no-op
    /// there, as it is on the synthesized backend.
    pub fn set_now_playing(&mut self, now: &crate::NowPlaying) {
        match &self.backend {
            #[cfg(web_page)]
            Backend::Web(w) => w.set_now_playing(now),
            _ => {
                let _ = now;
            }
        }
    }

    /// Transport requests from the system's now-playing controls that the
    /// engine cannot carry out itself (next and previous track). Play, pause
    /// and seeks are already applied when they are reported here.
    pub fn take_now_playing_actions(&mut self) -> Vec<crate::NowPlayingAction> {
        match &mut self.backend {
            #[cfg(web_page)]
            Backend::Web(w) => w.take_now_playing_actions(),
            _ => Vec::new(),
        }
    }

    /// Snapshot the engine's transport for a host to publish. `now_ms` is the
    /// wall-clock (unix millis) epoch the snapshot is authoritative at.
    pub fn transport_state(&self, now_ms: u64) -> TransportState {
        TransportState {
            playing: self.is_playing(),
            position_ms: self.position_ms(),
            rate: self.rate,
            epoch_ms: now_ms,
        }
    }

    /// Follower side: bring the engine in line with the host's `state`, using
    /// the projected playhead at `now_ms` and hard-seeking only when drift
    /// exceeds the threshold. Returns `true` if a corrective seek was issued.
    pub fn apply_transport(&mut self, state: &TransportState, now_ms: u64) -> bool {
        self.set_rate(state.rate);
        let target = state.projected_position(now_ms);
        let seeked = match correction(self.position_ms(), target) {
            Correction::Seek(ms) => {
                self.seek_ms(ms);
                true
            }
            Correction::InSync => false,
        };
        if state.playing && !self.is_playing() {
            self.play();
        } else if !state.playing && self.is_playing() {
            self.pause();
        }
        seeked
    }
}

impl Drop for MediaEngine {
    fn drop(&mut self) {
        crate::handoff::remove(self.handoff_id);
    }
}

impl Default for MediaEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// A decode build whose pipeline could not be made. It plays nothing and
/// paints nothing; each open fails with the reason, so a player shows that
/// instead of pretending to play.
#[cfg(gst_decode)]
struct UnavailableState {
    reason: String,
    /// An open whose failure the next poll reports.
    pending: bool,
}

#[cfg(gst_decode)]
impl UnavailableState {
    fn open(&mut self) {
        self.pending = true;
    }

    fn poll(&mut self) -> Vec<MediaEvent> {
        if std::mem::take(&mut self.pending) {
            vec![MediaEvent::Error(self.reason.clone())]
        } else {
            Vec::new()
        }
    }
}

// =====================================================================
// Synthesized fallback backend
// =====================================================================

#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent latches, not a state machine: `opened` and \
              `ended` are stream lifecycle, `playing` is transport, and \
              `announced_duration` records that one event has been emitted. \
              An enum would have to represent combinations this backend never \
              enters, and would say less than the four names do"
)]
struct SynthState {
    /// Whether a source has been opened. Until it has, the backend produces no
    /// frame at all so a freshly launched player shows a clean empty surface
    /// rather than a synthesized test pattern.
    opened: bool,
    playing: bool,
    rate: f32,
    position: u64,
    duration: u64,
    /// Wall-clock ms at the previous poll (baseline for advancing the
    /// playhead). `None` right after any state change so the next poll
    /// re-baselines instead of jumping across the gap.
    last_tick: Option<u64>,
    ended: bool,
    announced_duration: bool,
    latest: Option<VideoFrame>,
    /// The same picture as a [`Frame`], so `MediaEngine::latest` answers on
    /// both backends. The synthesized pattern is RGBA, so the two views share
    /// one allocation.
    latest_native: Option<Frame>,
    /// Position at which `latest` was last generated, so we only re-render the
    /// pattern when the playhead actually moved.
    last_frame_pos: Option<u64>,
    /// Prefetch progress for a file source.
    prefetch: Option<PrefetchProgress>,
    last_buffering: Option<u8>,
    /// A test's stand-in for a running pipeline that, after the next seek,
    /// hands over the picture it decoded for its old position: the timestamp
    /// that picture carries.
    #[cfg(test)]
    stale_on_seek: Option<u64>,
}

impl SynthState {
    fn new() -> Self {
        Self {
            opened: false,
            playing: false,
            rate: 1.0,
            position: 0,
            duration: DEFAULT_SYNTH_DURATION_MS,
            last_tick: None,
            ended: false,
            announced_duration: false,
            latest: None,
            latest_native: None,
            last_frame_pos: None,
            prefetch: None,
            last_buffering: None,
            #[cfg(test)]
            stale_on_seek: None,
        }
    }

    fn open(&mut self, source: &Source, _volume: f32) {
        self.opened = true;
        self.playing = false;
        self.position = 0;
        self.ended = false;
        self.announced_duration = false;
        self.last_tick = None;
        self.last_frame_pos = None;
        self.last_buffering = None;
        // Derive a plausible placeholder duration from the file size when we
        // have one (~125 KB/s), clamped to a supported range; decode replaces
        // this entirely. Remote / unknown sources get the default.
        self.duration = source.byte_len().map_or(DEFAULT_SYNTH_DURATION_MS, |b| {
            (b / 125).clamp(3_000, 3_600_000)
        });
        // Prefetch a file source through the ring to report hydration progress.
        // Remote sources have no local reader here.
        self.prefetch = match source.open_reader() {
            Ok(reader) => {
                let total = source.byte_len();
                // Spawning the prefetch thread can fail; hydration progress is
                // an optional readout, so a failure here leaves it unreported
                // rather than failing the open.
                Prefetcher::spawn(reader, total, 256 * 1024, 64 * 1024)
                    .ok()
                    .map(|p| p.progress())
            }
            Err(_) => None,
        };
        // Seed a frame at t=0 so an opened-but-not-yet-polled player isn't blank.
        self.render_frame();
    }

    fn set_playing(&mut self, playing: bool) {
        if playing && self.ended {
            // Restart from the top when told to play a finished stream.
            self.position = 0;
            self.ended = false;
        }
        self.playing = playing;
        self.last_tick = None; // re-baseline the clock across the state change
    }

    fn set_rate(&mut self, rate: f32) {
        self.rate = rate.clamp(0.25, 4.0);
        self.last_tick = None;
    }

    fn seek(&mut self, ms: u64) {
        self.position = ms.min(self.duration);
        self.ended = self.position >= self.duration;
        self.last_tick = None;
        if self.opened {
            self.render_frame();
            #[cfg(test)]
            if let Some(stale) = self.stale_on_seek.take()
                && let Some(frame) = &mut self.latest_native
            {
                frame.pts_ms = stale;
            }
        }
    }

    fn buffering_percent(&self) -> u8 {
        self.prefetch
            .as_ref()
            .map_or(100, super::ring::PrefetchProgress::percent)
    }

    fn poll_at(&mut self, now_ms: u64) -> Vec<MediaEvent> {
        let mut events = Vec::new();

        // 1. Duration is announced once, before any Position — a stable order
        //    a consumer can rely on.
        if !self.announced_duration {
            events.push(MediaEvent::DurationKnown { ms: self.duration });
            self.announced_duration = true;
        }

        // 2. Buffering, only on change.
        if self.prefetch.is_some() {
            let pct = self.buffering_percent();
            if self.last_buffering != Some(pct) {
                self.last_buffering = Some(pct);
                events.push(MediaEvent::Buffering { percent: pct });
            }
        }

        // 3. Advance the playhead while playing.
        if self.playing && !self.ended {
            if let Some(last) = self.last_tick {
                let dt = now_ms.saturating_sub(last);
                // A poll gap longer than `u32::MAX` ms is almost fifty days;
                // treating it as that upper bound avoids a lossy integer cast
                // while still making a stalled synthesized player catch up.
                let dt = u32::try_from(dt).unwrap_or(u32::MAX);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "both factors are finite and non-negative; the rate is bounded to 0.25..=4.0 and the fractional millisecond is intentionally discarded"
                )]
                let advance = (f64::from(dt) * f64::from(self.rate)) as u64;
                self.position = self.position.saturating_add(advance);
            }
            if self.position >= self.duration {
                self.position = self.duration;
                self.playing = false;
                self.ended = true;
                events.push(MediaEvent::Position { ms: self.position });
                events.push(MediaEvent::Ended);
            } else {
                events.push(MediaEvent::Position { ms: self.position });
            }
        }
        self.last_tick = Some(now_ms);

        // 4. Refresh the frame if the playhead moved — but only once a source is
        //    open, so a never-opened engine stays a clean empty surface.
        if self.opened && self.last_frame_pos != Some(self.position) {
            self.render_frame();
        }

        events
    }

    /// Where the playhead sits in the stream, `0.0..=1.0`. Zero while the
    /// duration is still unknown.
    fn playhead_fraction(&self) -> f64 {
        if self.duration == 0 {
            return 0.0;
        }
        // Both are millisecond counts, and `f64` holds every integer below
        // 2^53 — 285 000 years of milliseconds — exactly, so no stream that
        // can be played loses precision here.
        #[allow(
            clippy::cast_precision_loss,
            reason = "millisecond counts; f64 is exact to 2^53 ms, far past any playable stream"
        )]
        let frac = self.position as f64 / self.duration as f64;
        frac.clamp(0.0, 1.0)
    }

    /// Draw the synthesized picture for the current playhead: a calm diagonal
    /// gradient between two muted colours whose blend drifts with the
    /// playhead, so a capture shows a quiet picture that still changes as it
    /// plays.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "pixel coordinates are below 1000 and every channel is a blend \
                  of two bytes rounded back into 0..=255"
    )]
    fn render_frame(&mut self) {
        /// Dusk blue, top left.
        const FROM: [f64; 3] = [36.0, 48.0, 82.0];
        /// Muted teal, bottom right.
        const TO: [f64; 3] = [86.0, 120.0, 134.0];
        let mut rgba = vec![0u8; SYNTH_RGBA_BYTES];
        let drift = self.playhead_fraction() * 0.25;
        let span = f64::from(SYNTH_W + SYNTH_H - 2).max(1.0);
        for (y, row) in rgba.chunks_exact_mut(SYNTH_ROW_BYTES).enumerate() {
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let t = (x.saturating_add(y) as f64 / span * 0.75 + drift).clamp(0.0, 1.0);
                let [r, g, b] = std::array::from_fn::<u8, 3, _>(|i| {
                    let (from, to) = (FROM.get(i).copied(), TO.get(i).copied());
                    from.zip(to)
                        .map_or(0, |(from, to)| (from + (to - from) * t).round() as u8)
                });
                px.copy_from_slice(&[r, g, b, 255]);
            }
        }
        let frame = VideoFrame::new(SYNTH_W, SYNTH_H, rgba, self.position);
        self.latest_native = Some(Frame {
            width: SYNTH_W,
            height: SYNTH_H,
            pts_ms: self.position,
            payload: FramePayload::Rgba(frame.rgba.clone()),
        });
        self.latest = Some(frame);
        self.last_frame_pos = Some(self.position);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn synthesized_engine_transport_orders_events_and_follows_the_host() {
        {
            // "Block until the frame lands, however long that takes" is spelled
            // `Duration::MAX`. Turning that into a deadline overflows `Instant`,
            // and `Instant + Duration` panics rather than saturating — so the one
            // caller who asked hardest to be patient was the one who got killed.
            let mut e = MediaEngine::new_synthesized();
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            assert!(
                e.seek_and_wait(1_000, tilo_clock::Duration::MAX).is_ok(),
                "an unbounded wait still returns the frame it waited for"
            );
        }
        {
            let mut e = MediaEngine::new_synthesized();
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            e.play();

            // First poll: DurationKnown must precede any Position.
            let evts = e.poll_at(0);
            let dur_idx = evts
                .iter()
                .position(|x| matches!(x, MediaEvent::DurationKnown { .. }));
            assert!(
                dur_idx.is_some(),
                "duration announced on first poll: {evts:?}"
            );
            if let Some(pos_idx) = evts
                .iter()
                .position(|x| matches!(x, MediaEvent::Position { .. }))
            {
                assert!(dur_idx.unwrap() < pos_idx, "DurationKnown before Position");
            }
            let evts2 = e.poll_at(50);
            assert!(
                !evts2
                    .iter()
                    .any(|x| matches!(x, MediaEvent::DurationKnown { .. }))
            );
        }
        {
            let mut e = MediaEngine::new_synthesized();
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            e.play();
            e.poll_at(0); // baseline
            e.poll_at(1_000);
            assert_eq!(
                e.position_ms(),
                1_000,
                "1s of virtual time advanced the playhead 1s"
            );
            e.pause();
            e.poll_at(2_000);
            e.poll_at(3_000);
            assert_eq!(e.position_ms(), 1_000, "paused playhead does not advance");
        }
        {
            let mut engine = MediaEngine::new_synthesized();
            engine.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            engine.set_rate(2.0);
            engine.play();
            engine.poll_at(0);
            engine.poll_at(1_000);
            assert_eq!(engine.position_ms(), 2_000);
            assert!((engine.transport_state(1_000).rate - 2.0).abs() < f32::EPSILON);
            engine.set_rate(99.0);
            assert!((engine.rate() - 4.0).abs() < f32::EPSILON);
        }
        {
            let mut e = MediaEngine::new_synthesized();
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            e.seek_ms(e.duration_ms().unwrap() - 10);
            e.play();
            e.poll_at(0); // baseline
            let evts = e.poll_at(1_000); // jumps past the end
            assert_eq!(
                evts.last(),
                Some(&MediaEvent::Ended),
                "Ended is the final event"
            );
            assert!(
                evts.iter()
                    .any(|x| matches!(x, MediaEvent::Position { .. }))
            );
            assert!(!e.is_playing(), "stopped at the end");
            assert_eq!(e.position_ms(), e.duration_ms().unwrap());
        }
        {
            let mut e = MediaEngine::new_synthesized();
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            e.poll_at(0);
            // Host is playing, well ahead of us — a corrective seek must fire.
            let host = TransportState {
                playing: true,
                position_ms: 30_000,
                rate: 1.0,
                epoch_ms: 0,
            };
            let seeked = e.apply_transport(&host, 0);
            assert!(seeked, "large drift triggers a seek");
            assert_eq!(e.position_ms(), 30_000);
            assert!(e.is_playing(), "adopted the host's playing state");

            // Now in sync: a matching host state issues no seek.
            let host2 = TransportState {
                playing: true,
                position_ms: 30_000,
                rate: 1.0,
                epoch_ms: 0,
            };
            assert!(
                !e.apply_transport(&host2, 0),
                "no seek when already aligned"
            );
        }
        {
            let mut e = MediaEngine::new_synthesized();
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            e.seek_ms(5_000);
            e.play();
            let ts = e.transport_state(1_234);
            assert_eq!(ts.position_ms, 5_000);
            assert!(ts.playing);
            assert_eq!(ts.epoch_ms, 1_234);
        }
    }

    #[test]
    fn synthesized_engine_produces_frames_only_once_opened() {
        {
            let mut e = MediaEngine::new_synthesized();
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let f = e.latest_frame().expect("a seed frame exists after open");
            assert_eq!((f.width, f.height), (SYNTH_W, SYNTH_H));
            assert_eq!(f.rgba.len(), (SYNTH_W * SYNTH_H * 4) as usize);
        }
        {
            let mut e = MediaEngine::new_synthesized();
            // Pumping a never-opened engine must not fabricate a picture — the
            // player's empty state stays a clean surface, not a test pattern.
            e.poll_at(0);
            e.poll_at(100);
            assert!(
                e.latest_frame().is_none(),
                "empty state carries no synthesized frame"
            );
            // Opening a source seeds the first frame.
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            assert!(
                e.latest_frame().is_some(),
                "a frame appears once media is opened"
            );
        }
    }
}
