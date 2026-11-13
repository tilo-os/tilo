//! The playback engine: one control surface over a `GStreamer` decode backend
//! (`--features decode`) or a synthesized test pattern. The public surface is the
//! same either way; a consumer declares the memory it can paint through
//! [`EngineConfig`] and [`FrameAcceptance`].

use std::sync::OnceLock;
use tilo_clock::time::Instant;

use crate::event::{MediaEvent, VideoFrame};
use crate::frame::{Frame, FrameAcceptance, FramePayload};
use crate::hw::{HwPolicy, HwReport};
use crate::ring::{PrefetchProgress, Prefetcher};
use crate::source::Source;
use crate::transport::{Correction, TransportState, correction};

/// A selectable elementary stream reported by the decoder. The label is the
/// container's, else `Audio track N` — never a guessed language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaTrack {
    pub id: String,
    pub index: u32,
    pub label: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaChapter {
    pub id: String,
    pub label: String,
    pub start_ms: u64,
}

#[cfg(gst_decode)]
pub use crate::gst_backend::SeekMode;

/// Default seek precision. Exact: an editor scrubbing to 12.500 s must see the
/// frame at 12.500 s, not a keyframe up to two seconds behind.
#[cfg(not(gst_decode))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekMode {
    Accurate,
    Keyframe,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    pub acceptance: FrameAcceptance,
    pub hw_policy: HwPolicy,
    pub volume: f32,
    pub audio_output: bool,
    pub video_output: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            acceptance: FrameAcceptance::Rgba,
            hw_policy: HwPolicy::from_env(),
            volume: 0.8,
            audio_output: true,
            video_output: true,
        }
    }
}

impl EngineConfig {
    pub fn gpu() -> Self {
        Self {
            acceptance: FrameAcceptance::Native,
            ..Self::default()
        }
    }

    /// Start muted: previews and thumbnails must never make a sound.
    #[must_use]
    pub fn silent(mut self) -> Self {
        self.volume = 0.0;
        self
    }

    /// Decode pictures only: no audio stream, no audio device, so a missing or busy
    /// audio server never stops a picture.
    #[must_use]
    pub fn frames_only(mut self) -> Self {
        self.volume = 0.0;
        self.audio_output = false;
        self
    }

    /// Play sound only; the picture is never shown.
    #[must_use]
    pub fn audio_only(mut self) -> Self {
        self.video_output = false;
        self
    }

    #[must_use]
    pub fn software_only(mut self) -> Self {
        self.hw_policy = HwPolicy::Off;
        self
    }

    #[must_use]
    pub fn with_hw_policy(mut self, policy: HwPolicy) -> Self {
        self.hw_policy = policy;
        self
    }
}

const DEFAULT_SYNTH_DURATION_MS: u64 = 120_000;

const SYNTH_W: u32 = 480;
const SYNTH_H: u32 = 270;
const SYNTH_ROW_BYTES: usize = SYNTH_W as usize * 4;
const SYNTH_RGBA_BYTES: usize = SYNTH_ROW_BYTES * SYNTH_H as usize;

fn mono_now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    // Saturating, not truncating: a playhead must never wrap.
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Why [`MediaEngine::seek_and_wait`] returned without a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeekWaitError {
    Failed(String),
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

pub struct MediaEngine {
    handoff_id: u64,
    handoff_controller_bus_name: Option<String>,
    backend: Backend,
    source: Option<Source>,
    volume: f32,
    muted: bool,
    rate: f32,
    config: EngineConfig,
    now_playing: Option<crate::NowPlaying>,
}

enum Backend {
    Synth(SynthState),
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

    pub fn set_handoff_controller(&mut self, bus_name: Option<String>) {
        self.handoff_controller_bus_name = bus_name;
        self.refresh_handoff();
    }

    /// Start the decode backend's one-time process set-up in the background so the
    /// first engine built finds it done. A no-op without the decode backend and after
    /// the first call.
    pub fn prepare_in_background() {
        #[cfg(gst_decode)]
        crate::gst_backend::prepare_in_background();
    }

    /// Build an engine with the default configuration: system-memory RGBA frames,
    /// hardware decode per `TILO_MEDIA_HWDEC`. In a decode build whose pipeline cannot
    /// be made, [`Self::is_real_decode`] is `false` and every open fails with the
    /// reason; it never falls back to synthesized pictures. Never panics.
    pub fn new() -> Self {
        Self::with_config(EngineConfig::default())
    }

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
                    now_playing: None,
                };
            }
        }
        #[cfg(gst_decode)]
        let backend = match crate::gst_backend::GstState::new(
            config.acceptance,
            config.hw_policy,
            config.audio_output,
            config.video_output,
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
            now_playing: None,
        }
    }

    /// Test seam: the synthesized backend answers its next seek with a picture
    /// stamped `pts_ms`, as a pipeline mid-playback hands over its current frame.
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

    /// Force the synthesized backend regardless of build features.
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
            now_playing: None,
        }
    }

    pub fn config(&self) -> EngineConfig {
        self.config
    }

    /// What the decode pipeline plugged: decoder name, family, and the caps the sink
    /// negotiated. Empty, with [`HwReport::is_hardware`] `false`, until a frame prerolls.
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

    pub fn is_decode_compiled() -> bool {
        cfg!(any(gst_decode, web_page))
    }

    pub fn source(&self) -> Option<&Source> {
        self.source.as_ref()
    }

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
        // Handed over, not cloned: no backend reads `self.source`.
        self.source = Some(source);
        self.refresh_handoff();
    }

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

    pub fn toggle(&mut self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

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
        timeout: tilo_clock::time::Duration,
    ) -> Result<(), SeekWaitError> {
        // A `timeout` of `Duration::MAX` asks to pump until the frame lands;
        // `Instant + Duration` would panic on it. A page cannot block on its own
        // event loop, so there it gets one attempt and reports what landed.
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
        let deadline = tilo_clock::now().checked_add(timeout);
        // A pipeline that has not prerolled refuses seeks, so repeat until one is
        // taken.
        let mut accepted = self.seek_with(ms, SeekMode::Accurate);
        loop {
            #[cfg(gst_decode)]
            if let Backend::Gst(g) = &mut self.backend {
                // A flushing seek is asynchronous: the new preroll arrives when the state
                // change completes.
                g.wait_settled(tilo_clock::time::Duration::from_millis(50));
            }
            if !accepted {
                accepted = self.seek_with(ms, SeekMode::Accurate);
            }
            if let Some(reason) = Self::failure(self.poll()) {
                return Err(SeekWaitError::Failed(reason));
            }
            if accepted && self.frame_is_current() {
                return Ok(());
            }
            if deadline.is_some_and(|d| tilo_clock::now() >= d) {
                return Err(SeekWaitError::TimedOut);
            }
            std::thread::sleep(tilo_clock::time::Duration::from_millis(5));
        }
    }

    fn failure(events: Vec<MediaEvent>) -> Option<String> {
        events.into_iter().find_map(|event| match event {
            MediaEvent::Error(reason) => Some(reason),
            _ => None,
        })
    }

    /// Set output volume in `0.0..=1.0`. Applied to a dedicated gain element so
    /// changing it does not re-negotiate the stream; muting is separate and keeps
    /// this level.
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

    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// Empty for the synthesized backend and for video-only media.
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

    /// Empty when the container supplied no chapter metadata.
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

    /// The audio sink element, for routing or monitoring this source's output: a
    /// `PipeWire` target node, a level meter, a tee into a mixer. `None` on the
    /// synthesized backend and when the audio chain could not be built.
    #[cfg(gst_decode)]
    pub fn audio_sink(&self) -> Option<gstreamer::Element> {
        match &self.backend {
            Backend::Synth(_) | Backend::Unavailable(_) => None,
            Backend::Gst(g) => g.audio_sink(),
        }
    }

    /// Decoded audio the output has rendered since the last open: the device's
    /// progress when the output is governed, else the buffers that reached the
    /// sink. Zero without the decode backend.
    pub fn audio_buffers_rendered(&self) -> u64 {
        match &self.backend {
            #[cfg(gst_decode)]
            Backend::Gst(g) => g.audio_buffers_rendered(),
            _ => 0,
        }
    }

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

    /// The newest frame as system-memory RGBA. Under [`FrameAcceptance::Native`] a
    /// picture may be a dmabuf or planar YUV, and this is then `None`: reading it
    /// must not trigger a hidden readback. Those consumers use [`Self::latest`].
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

    pub fn prefer_element_pictures(&mut self) {
        #[cfg(web_page)]
        if let Backend::Web(w) = &mut self.backend {
            w.prefer_element_pictures();
        }
    }

    pub fn take_element_picture(&mut self) -> Option<crate::ElementPicture> {
        match &mut self.backend {
            #[cfg(web_page)]
            Backend::Web(w) => w.take_element_picture(),
            _ => None,
        }
    }

    /// Whether the held picture belongs to the most recent seek rather than to the
    /// position before it. A scrub keeps painting the previous frame while the next
    /// one decodes.
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

    /// The newest picture in whatever memory the decoder produced it in: a dmabuf
    /// for zero-copy import, NV12/P010 planar, or the packed RGBA buffer
    /// [`Self::latest_frame`] also exposes.
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

    /// Test seam: pump at an explicit virtual `now_ms`. On a decode backend this
    /// behaves like [`Self::poll`] and ignores the argument.
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

    /// Describe what is playing to the system's now-playing controls: the page's
    /// Media Session on the web, the widget's MPRIS service on Linux, which sends it
    /// without this. An unchanged description is not sent again.
    pub fn set_now_playing(&mut self, now: &crate::NowPlaying) {
        if self.now_playing.as_ref() == Some(now) {
            return;
        }
        self.now_playing = Some(now.clone());
        #[cfg(web_page)]
        if let Backend::Web(w) = &self.backend {
            w.set_now_playing(now);
        }
    }

    #[must_use]
    pub fn now_playing(&self) -> Option<&crate::NowPlaying> {
        self.now_playing.as_ref()
    }

    /// Transport requests from the now-playing controls that the engine cannot
    /// carry out itself (next, previous). Play, pause and seeks are applied before
    /// they are reported here.
    pub fn take_now_playing_actions(&mut self) -> Vec<crate::NowPlayingAction> {
        match &mut self.backend {
            #[cfg(web_page)]
            Backend::Web(w) => w.take_now_playing_actions(),
            Backend::Synth(s) => std::mem::take(&mut s.now_playing_actions),
            #[cfg(gst_decode)]
            Backend::Gst(_) | Backend::Unavailable(_) => Vec::new(),
        }
    }

    #[allow(
        irrefutable_let_patterns,
        reason = "a build without a decode or page backend has only the synthesized one"
    )]
    pub fn press_now_playing(&mut self, action: crate::NowPlayingAction) {
        if let Backend::Synth(s) = &mut self.backend {
            s.now_playing_actions.push(action);
        }
    }

    pub fn transport_state(&self, now_ms: u64) -> TransportState {
        TransportState {
            playing: self.is_playing(),
            position_ms: self.position_ms(),
            rate: self.rate,
            epoch_ms: now_ms,
        }
    }

    /// Follower side: bring the engine in line with the host's `state` at the
    /// projected playhead, hard-seeking only past the drift threshold. `true` if a
    /// seek was issued.
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

/// A decode build whose pipeline could not be made: every open fails with the
/// reason, so a player shows that instead of pretending to play.
#[cfg(gst_decode)]
struct UnavailableState {
    reason: String,
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent latches, not a state machine: `opened` and \
              `ended` are stream lifecycle, `playing` is transport, and \
              `announced_duration` records that one event has been emitted. \
              An enum would have to represent combinations this backend never \
              enters, and would say less than the four names do"
)]
struct SynthState {
    /// Until a source is opened the backend produces no frame at all, so a freshly
    /// launched player shows an empty surface.
    opened: bool,
    playing: bool,
    rate: f32,
    position: u64,
    duration: u64,
    /// Wall-clock ms at the previous poll. `None` after any state change, so the
    /// next poll re-baselines instead of jumping the gap.
    last_tick: Option<u64>,
    ended: bool,
    announced_duration: bool,
    latest: Option<VideoFrame>,
    /// The same picture as a [`Frame`], so `latest` answers on both backends. RGBA,
    /// so both views share one allocation.
    latest_native: Option<Frame>,
    last_frame_pos: Option<u64>,
    prefetch: Option<PrefetchProgress>,
    last_buffering: Option<u8>,
    /// Presses of the now-playing controls ([`MediaEngine::press_now_playing`]) not
    /// taken yet.
    now_playing_actions: Vec<crate::NowPlayingAction>,
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
            now_playing_actions: Vec::new(),
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
        self.duration = source.byte_len().map_or(DEFAULT_SYNTH_DURATION_MS, |b| {
            (b / 125).clamp(3_000, 3_600_000)
        });
        self.prefetch = match source.open_reader() {
            Ok(reader) => {
                let total = source.byte_len();
                // A failed prefetch thread leaves hydration progress unreported rather than
                // failing the open.
                Prefetcher::spawn(reader, total, 256 * 1024, 64 * 1024)
                    .ok()
                    .map(|p| p.progress())
            }
            Err(_) => None,
        };
        // Seed a frame at t=0 so an opened but unpolled player is not blank.
        self.render_frame();
    }

    fn set_playing(&mut self, playing: bool) {
        if playing && self.ended {
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
        // Milliseconds fit `f64` exactly below 2^53, so no playable stream loses
        // precision here.
        #[allow(
            clippy::cast_precision_loss,
            reason = "millisecond counts; f64 is exact to 2^53 ms, far past any playable stream"
        )]
        let frac = self.position as f64 / self.duration as f64;
        frac.clamp(0.0, 1.0)
    }

    // A calm diagonal gradient between muted colours, drifting with the playhead.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "pixel coordinates are below 1000 and every channel is a blend \
                  of two bytes rounded back into 0..=255"
    )]
    fn render_frame(&mut self) {
        const FROM: [f64; 3] = [36.0, 48.0, 82.0];
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

    fn opened() -> MediaEngine {
        let mut engine = MediaEngine::new_synthesized();
        engine.open(Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        engine
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn synthesized_engine_transport_orders_events_and_follows_the_host() {
        {
            // `Duration::MAX` asks to wait for the frame however long it takes, which
            // overflows `Instant`.
            let mut e = opened();
            assert!(
                e.seek_and_wait(1_000, tilo_clock::time::Duration::MAX)
                    .is_ok(),
                "an unbounded wait still returns the frame it waited for"
            );
        }
        {
            let mut e = opened();
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
            let mut e = opened();
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
            let mut engine = opened();
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
            let mut e = opened();
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
            let mut e = opened();
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
            let mut e = opened();
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
            let e = opened();
            let f = e.latest_frame().expect("a seed frame exists after open");
            assert_eq!((f.width, f.height), (SYNTH_W, SYNTH_H));
            assert_eq!(f.rgba.len(), (SYNTH_W * SYNTH_H * 4) as usize);
        }
        {
            let mut e = MediaEngine::new_synthesized();
            // A never-opened engine must not fabricate a picture.
            e.poll_at(0);
            e.poll_at(100);
            assert!(
                e.latest_frame().is_none(),
                "empty state carries no synthesized frame"
            );
            e.open(Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            assert!(
                e.latest_frame().is_some(),
                "a frame appears once media is opened"
            );
        }
    }

    #[test]
    fn the_engine_keeps_what_it_described_and_reports_the_controls_presses() {
        let mut engine = MediaEngine::new_synthesized();
        assert_eq!(engine.now_playing(), None);
        let song = crate::NowPlaying {
            title: "Harbour".into(),
            artist: "Ada".into(),
            ..crate::NowPlaying::default()
        };
        engine.set_now_playing(&song);
        assert_eq!(engine.now_playing(), Some(&song));
        engine.press_now_playing(crate::NowPlayingAction::Next);
        assert_eq!(
            engine.take_now_playing_actions(),
            vec![crate::NowPlayingAction::Next]
        );
        assert!(engine.take_now_playing_actions().is_empty(), "taken once");
    }
}
