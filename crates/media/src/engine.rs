//! The playback engine: one control surface, two interchangeable backends.
//!
//! * **Real decode** (`--features decode`) drives a `GStreamer` `playbin` whose
//!   video is diverted through `videoconvert ! appsink(RGBA)` for decoded
//!   frames and whose audio is routed to a real system sink (`autoaudiosink`) —
//!   the audible + visible path, mirroring `apps/music`'s real sink chain and
//!   `apps/tube`'s appsink. This decodes **any** codec `GStreamer` can plug
//!   (H.264 / VP9 / AV1) from a local file, the FUSE workspace, or a stream URL.
//! * **Synthesized fallback** (default) produces a deterministic RGBA test
//!   pattern driven by a real playhead clock, plus genuine buffering progress
//!   from the prefetcher when a file source is opened. It needs no gstreamer
//!   stack, so hermetic builds, `--shots` and CI stay green.
//!
//! The public surface ([`MediaEngine`]) is identical either way, so a consumer
//! never branches on which backend is linked.

use std::sync::OnceLock;
use std::time::Instant;

use crate::event::{MediaEvent, VideoFrame};
use crate::ring::{PrefetchProgress, Prefetcher};
use crate::source::Source;
use crate::transport::{Correction, TransportState, correction};

/// Placeholder stream length for the synthesized fallback when nothing better
/// is known. Real decode always overrides this from the pipeline's duration.
const DEFAULT_SYNTH_DURATION_MS: u64 = 120_000;

/// Synthesized-frame dimensions. 16:9, small enough to regenerate cheaply each
/// frame, large enough to read as "a video" in a `--shots` capture.
const SYNTH_W: u32 = 480;
const SYNTH_H: u32 = 270;

/// Process-monotonic millisecond clock for the fallback playhead. A lazily
/// pinned `Instant` base keeps it cheap and immune to wall-clock jumps.
fn mono_now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed_ms = START.get_or_init(Instant::now).elapsed().as_millis();
    u64::try_from(elapsed_ms).unwrap_or(u64::MAX)
}

/// A collaborative, source-agnostic media engine. Construct with [`Self::new`],
/// point it at a [`Source`] with [`Self::open`], drive it with `play` / `pause`
/// / `seek_ms` / `set_volume`, and pump it once per frame with [`Self::poll`],
/// which returns the [`MediaEvent`]s since the last call and refreshes
/// [`Self::latest_frame`].
pub struct MediaEngine {
    backend: Backend,
    source: Option<Source>,
    volume: f32,
}

enum Backend {
    Synth(SynthState),
    #[cfg(feature = "decode")]
    Gst(gst_backend::GstState),
}

impl MediaEngine {
    /// Build an engine. With `--features decode` this stands up a real
    /// `GStreamer` pipeline; if `GStreamer` can't initialise (or the feature is
    /// off) it transparently falls back to the synthesized backend. Never
    /// fails.
    pub fn new() -> Self {
        #[cfg(feature = "decode")]
        {
            if let Some(gst) = gst_backend::GstState::new() {
                return Self {
                    backend: Backend::Gst(gst),
                    source: None,
                    volume: 0.8,
                };
            }
        }
        Self {
            backend: Backend::Synth(SynthState::new()),
            source: None,
            volume: 0.8,
        }
    }

    /// Force the synthesized backend regardless of build features. Used by the
    /// hermetic engine tests so event ordering and transport application are
    /// exercised deterministically without a real decode.
    pub fn new_synthesized() -> Self {
        Self {
            backend: Backend::Synth(SynthState::new()),
            source: None,
            volume: 0.8,
        }
    }

    /// Whether this engine is decoding through a real `GStreamer` pipeline (vs.
    /// the synthesized fallback).
    pub fn is_real_decode(&self) -> bool {
        match &self.backend {
            Backend::Synth(_) => false,
            #[cfg(feature = "decode")]
            Backend::Gst(_) => true,
        }
    }

    /// Whether real decode is even compiled into this build.
    pub fn is_decode_compiled() -> bool {
        cfg!(feature = "decode")
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
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.open(&source, self.volume),
        }
        self.source = Some(source);
    }

    /// Start (or resume) playback.
    pub fn play(&mut self) {
        match &mut self.backend {
            Backend::Synth(s) => s.set_playing(true),
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.set_playing(true),
        }
    }

    /// Pause playback, holding the playhead in place.
    pub fn pause(&mut self) {
        match &mut self.backend {
            Backend::Synth(s) => s.set_playing(false),
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.set_playing(false),
        }
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
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.is_playing(),
        }
    }

    /// Seek to an absolute playhead position in milliseconds (clamped to the
    /// known duration).
    pub fn seek_ms(&mut self, ms: u64) {
        match &mut self.backend {
            Backend::Synth(s) => s.seek(ms),
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.seek(ms),
        }
    }

    /// Set output volume in `0.0..=1.0`.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        match &mut self.backend {
            Backend::Synth(_) => {}
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.set_volume(self.volume),
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// The current playhead in milliseconds.
    pub fn position_ms(&self) -> u64 {
        match &self.backend {
            Backend::Synth(s) => s.position,
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.position_ms(),
        }
    }

    /// The total duration in milliseconds, once known.
    pub fn duration_ms(&self) -> Option<u64> {
        match &self.backend {
            Backend::Synth(s) => Some(s.duration),
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.duration_ms(),
        }
    }

    /// Buffering / prefetch progress, 0..=100.
    pub fn buffering_percent(&self) -> u8 {
        match &self.backend {
            Backend::Synth(s) => s.buffering_percent(),
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.buffering_percent(),
        }
    }

    /// The most recently produced frame, if any.
    pub fn latest_frame(&self) -> Option<&VideoFrame> {
        match &self.backend {
            Backend::Synth(s) => s.latest.as_ref(),
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.latest_frame(),
        }
    }

    /// Pump the engine: pull any new frame, drain backend events, advance the
    /// playhead. Returns the [`MediaEvent`]s produced since the last call.
    /// Call once per rendered frame.
    pub fn poll(&mut self) -> Vec<MediaEvent> {
        match &mut self.backend {
            Backend::Synth(s) => s.poll_at(mono_now_ms()),
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.poll(),
        }
    }

    /// Test seam: pump the synthesized backend at an explicit virtual `now_ms`
    /// so event ordering is deterministic without real time. On a real-decode
    /// backend this behaves like [`Self::poll`] (the argument is ignored).
    pub fn poll_at(&mut self, now_ms: u64) -> Vec<MediaEvent> {
        match &mut self.backend {
            Backend::Synth(s) => s.poll_at(now_ms),
            #[cfg(feature = "decode")]
            Backend::Gst(g) => g.poll(),
        }
    }

    // --- Collaborative transport ------------------------------------------

    /// Snapshot the engine's transport for a host to publish. `now_ms` is the
    /// wall-clock (unix millis) epoch the snapshot is authoritative at.
    pub fn transport_state(&self, now_ms: u64) -> TransportState {
        TransportState {
            playing: self.is_playing(),
            position_ms: self.position_ms(),
            rate: 1.0,
            epoch_ms: now_ms,
        }
    }

    /// Follower side: bring the engine in line with the host's `state`, using
    /// the projected playhead at `now_ms` and hard-seeking only when drift
    /// exceeds the threshold. Returns `true` if a corrective seek was issued.
    pub fn apply_transport(&mut self, state: &TransportState, now_ms: u64) -> bool {
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

impl Default for MediaEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =====================================================================
// Synthesized fallback backend
// =====================================================================

#[allow(
    clippy::struct_excessive_bools,
    reason = "opened/playing/ended/announced_duration are independent flags on \
              distinct concerns (source loaded, transport running, stream \
              finished, one-shot event emitted), not a state machine that would \
              collapse into an enum"
)]
struct SynthState {
    /// Whether a source has been opened. Until it has, the backend produces no
    /// frame at all so a freshly launched player shows a clean empty surface
    /// rather than a synthesized test pattern.
    opened: bool,
    playing: bool,
    position: u64,
    duration: u64,
    /// Wall-clock ms at the previous poll (baseline for advancing the
    /// playhead). `None` right after any state change so the next poll
    /// re-baselines instead of jumping across the gap.
    last_tick: Option<u64>,
    ended: bool,
    announced_duration: bool,
    latest: Option<VideoFrame>,
    /// Position at which `latest` was last generated, so we only re-render the
    /// pattern when the playhead actually moved.
    last_frame_pos: Option<u64>,
    /// Prefetch progress for a file source (genuine hydration percentage).
    prefetch: Option<PrefetchProgress>,
    last_buffering: Option<u8>,
}

impl SynthState {
    fn new() -> Self {
        Self {
            opened: false,
            playing: false,
            position: 0,
            duration: DEFAULT_SYNTH_DURATION_MS,
            last_tick: None,
            ended: false,
            announced_duration: false,
            latest: None,
            last_frame_pos: None,
            prefetch: None,
            last_buffering: None,
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
        // have one (~125 KB/s), clamped to a sane range; real decode replaces
        // this entirely. Remote / unknown sources get the default.
        self.duration = source.byte_len().map_or(DEFAULT_SYNTH_DURATION_MS, |b| {
            (b / 125).clamp(3_000, 3_600_000)
        });
        // Prefetch a file source through the ring so buffering is real
        // hydration progress. Remote sources have no local reader here.
        self.prefetch = match source.open_reader() {
            Ok(reader) => {
                let total = source.byte_len();
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

    fn seek(&mut self, ms: u64) {
        self.position = ms.min(self.duration);
        self.ended = self.position >= self.duration;
        self.last_tick = None;
        if self.opened {
            self.render_frame();
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
                self.position = self.position.saturating_add(dt);
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

    /// Draw the synthesized test pattern for the current playhead: a slowly
    /// drifting hue gradient with diagonal content stripes and a bright scan
    /// bar at the playhead fraction, so a capture unmistakably shows a moving
    /// picture and the correct progress.
    #[allow(
        clippy::many_single_char_names,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        clippy::arithmetic_side_effects,
        reason = "pixel maths for the synthesized test pattern: x/y/w/h/r/g/b are \
                  conventional single-letter names for this domain; scan_x is \
                  bounded to 0..w by frac's 0.0..=1.0 clamp; position/duration \
                  are millisecond counts far inside f64's exact-integer range for \
                  any real playback session; and the RGB channel casts \
                  intentionally wrap mod 256 to produce the cyclic hue-drift the \
                  doc comment above describes, not a truncation bug"
    )]
    fn render_frame(&mut self) {
        let w = SYNTH_W as usize;
        let h = SYNTH_H as usize;
        let row_bytes = w.saturating_mul(4);
        let mut rgba = vec![0u8; row_bytes.saturating_mul(h)];
        let frac = if self.duration > 0 {
            (self.position as f64 / self.duration as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let scan_x = (frac * (w as f64 - 1.0)) as usize;
        let hue_shift = (self.position / 40) as usize;
        for (y, row) in rgba.chunks_exact_mut(row_bytes).enumerate() {
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let Ok(px): Result<&mut [u8; 4], _> = px.try_into() else {
                    continue;
                };
                // Base gradient: x drives red/green, y drives blue; hue_shift
                // slides it so the pattern visibly animates over time.
                let r = ((x.wrapping_add(hue_shift).wrapping_mul(255)) / w) as u8;
                let g = ((y.wrapping_mul(255)) / h) as u8;
                let b = ((x.wrapping_add(y).wrapping_add(hue_shift).wrapping_mul(255))
                    / w.wrapping_add(h)) as u8;
                // Diagonal content stripes for texture.
                let stripe = if (x.wrapping_add(y).wrapping_add(hue_shift) / 24).is_multiple_of(2) {
                    24u8
                } else {
                    0u8
                };
                px[0] = r.wrapping_add(stripe);
                px[1] = g;
                px[2] = b;
                px[3] = 255;
                // Bright scan bar at the playhead.
                if x.abs_diff(scan_x) <= 1 {
                    px[0] = 255;
                    px[1] = 255;
                    px[2] = 255;
                }
            }
        }
        self.latest = Some(VideoFrame::new(SYNTH_W, SYNTH_H, rgba, self.position));
        self.last_frame_pos = Some(self.position);
    }
}

// =====================================================================
// Real GStreamer backend (only compiled with `--features decode`)
// =====================================================================

#[cfg(feature = "decode")]
mod gst_backend {
    use super::{MediaEvent, Source, VideoFrame};
    use gstreamer as gst;
    use gstreamer::prelude::*;
    use gstreamer_app as gst_app;
    use gstreamer_video as gst_video;
    use std::collections::VecDeque;

    pub struct GstState {
        playbin: gst::Element,
        appsink: gst_app::AppSink,
        bus: gst::Bus,
        latest: Option<VideoFrame>,
        playing: bool,
        duration_announced: bool,
        last_position: Option<u64>,
        last_buffering: Option<u8>,
        buffering: u8,
        pending: VecDeque<MediaEvent>,
    }

    impl GstState {
        /// Build the pipeline with no source yet. Returns `None` if `GStreamer`
        /// can't initialise or the `playbin` / sink chain can't be built, so
        /// the engine falls back to the synthesized backend.
        pub fn new() -> Option<Self> {
            gst::init().ok()?;
            let playbin = gst::ElementFactory::make("playbin").build().ok()?;

            // Divert video to an RGBA appsink (`sync=true` paces frames to PTS).
            let video_bin = gst::parse::bin_from_description(
                "videoconvert ! appsink name=qos_vsink caps=video/x-raw,format=RGBA \
                 sync=true max-buffers=4 drop=true",
                true,
            )
            .ok()?;
            let appsink = video_bin
                .by_name("qos_vsink")?
                .downcast::<gst_app::AppSink>()
                .ok()?;
            playbin.set_property("video-sink", &video_bin);

            // Route audio through an explicit real sink chain so playback is
            // audible (autoaudiosink plugs pipewire/pulse/alsa) — never a
            // fakesink. If the elements are missing, leave playbin's default.
            if let Ok(audio_bin) = gst::parse::bin_from_description(
                "audioconvert ! audioresample ! autoaudiosink name=qos_asink sync=true",
                true,
            ) {
                playbin.set_property("audio-sink", &audio_bin);
            }
            playbin.set_property("mute", false);
            playbin.set_property("volume", 0.8f64);

            let bus = playbin.bus()?;
            Some(Self {
                playbin,
                appsink,
                bus,
                latest: None,
                playing: false,
                duration_announced: false,
                last_position: None,
                last_buffering: None,
                buffering: 100,
                pending: VecDeque::new(),
            })
        }

        pub fn open(&mut self, source: &Source, volume: f32) {
            let _ = self.playbin.set_state(gst::State::Null);
            self.latest = None;
            self.playing = false;
            self.duration_announced = false;
            self.last_position = None;
            self.last_buffering = None;
            self.buffering = 100;
            self.pending.clear();
            self.playbin.set_property("uri", source.to_uri());
            self.playbin.set_property("mute", false);
            self.playbin.set_property("volume", f64::from(volume));
            // Preroll so a first frame + the duration become available before
            // the user presses play.
            let _ = self.playbin.set_state(gst::State::Paused);
        }

        pub fn set_playing(&mut self, playing: bool) {
            self.playing = playing;
            let state = if playing {
                gst::State::Playing
            } else {
                gst::State::Paused
            };
            let _ = self.playbin.set_state(state);
        }

        pub fn is_playing(&self) -> bool {
            self.playing
        }

        pub fn seek(&self, ms: u64) {
            let _ = self.playbin.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_mseconds(ms),
            );
        }

        pub fn set_volume(&self, v: f32) {
            self.playbin.set_property("volume", f64::from(v));
        }

        pub fn position_ms(&self) -> u64 {
            self.playbin
                .query_position::<gst::ClockTime>()
                .map(gstreamer::ClockTime::mseconds)
                .or(self.last_position)
                .or_else(|| self.latest.as_ref().map(|f| f.pts_ms))
                .unwrap_or(0)
        }

        pub fn duration_ms(&self) -> Option<u64> {
            self.playbin
                .query_duration::<gst::ClockTime>()
                .map(gstreamer::ClockTime::mseconds)
        }

        pub fn buffering_percent(&self) -> u8 {
            self.buffering
        }

        pub fn latest_frame(&self) -> Option<&VideoFrame> {
            self.latest.as_ref()
        }

        pub fn poll(&mut self) -> Vec<MediaEvent> {
            let mut events: Vec<MediaEvent> = self.pending.drain(..).collect();

            // Duration, announced once as soon as it is known.
            if !self.duration_announced
                && let Some(ms) = self.duration_ms()
            {
                events.push(MediaEvent::DurationKnown { ms });
                self.duration_announced = true;
            }

            // Drain bus messages (buffering / eos / error).
            while let Some(msg) = self.bus.pop() {
                use gst::MessageView;
                match msg.view() {
                    MessageView::Buffering(b) => {
                        let pct = u8::try_from(b.percent().clamp(0, 100)).unwrap_or(100);
                        self.buffering = pct;
                        if self.last_buffering != Some(pct) {
                            self.last_buffering = Some(pct);
                            events.push(MediaEvent::Buffering { percent: pct });
                        }
                    }
                    MessageView::Eos(_) => {
                        self.playing = false;
                        events.push(MediaEvent::Ended);
                    }
                    MessageView::Error(e) => {
                        self.playing = false;
                        events.push(MediaEvent::Error(e.error().to_string()));
                    }
                    _ => {}
                }
            }

            // Pull the freshest decoded frame(s); keep only the last so the
            // surface never lags behind the pipeline.
            while let Some(sample) = self.appsink.try_pull_sample(Some(gst::ClockTime::ZERO)) {
                if let Some(frame) = frame_from_sample(&sample) {
                    self.latest = Some(frame);
                }
            }

            // Position, emitted when it advances.
            let pos = self.position_ms();
            if self.last_position != Some(pos) {
                self.last_position = Some(pos);
                events.push(MediaEvent::Position { ms: pos });
            }

            events
        }
    }

    impl Drop for GstState {
        fn drop(&mut self) {
            let _ = self.playbin.set_state(gst::State::Null);
        }
    }

    /// Copy one RGBA appsink sample into an owned, tightly-packed [`VideoFrame`],
    /// honouring `GStreamer`'s row stride (which may pad past `width * 4`).
    fn frame_from_sample(sample: &gst::Sample) -> Option<VideoFrame> {
        let caps = sample.caps()?;
        let info = gst_video::VideoInfo::from_caps(caps).ok()?;
        let buffer = sample.buffer()?;
        let pts_ms = buffer.pts().map_or(0, gstreamer::ClockTime::mseconds);
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;

        let w = info.width() as usize;
        let h = info.height() as usize;
        let stride = usize::try_from(*info.stride().first()?).ok()?;
        let data = frame.plane_data(0).ok()?;
        let row = w.checked_mul(4)?;
        if stride < row || data.len() < stride.checked_mul(h)? {
            return None;
        }
        let mut rgba = Vec::with_capacity(row.checked_mul(h)?);
        for y in 0..h {
            let start = y.checked_mul(stride)?;
            let end = start.checked_add(row)?;
            rgba.extend_from_slice(data.get(start..end)?);
        }
        Some(VideoFrame::new(info.width(), info.height(), rgba, pts_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_orders_duration_before_position() {
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
        // Duration announced exactly once.
        let evts2 = e.poll_at(50);
        assert!(
            !evts2
                .iter()
                .any(|x| matches!(x, MediaEvent::DurationKnown { .. }))
        );
    }

    #[test]
    fn playhead_advances_with_virtual_time() {
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

    #[test]
    fn reaching_duration_emits_position_then_ended_last() {
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

    #[test]
    fn synthesized_engine_produces_a_frame() {
        let mut e = MediaEngine::new_synthesized();
        e.open(Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        let f = e.latest_frame().expect("a seed frame exists after open");
        assert_eq!((f.width, f.height), (SYNTH_W, SYNTH_H));
        assert_eq!(f.rgba.len(), (SYNTH_W * SYNTH_H * 4) as usize);
    }

    #[test]
    fn no_frame_is_produced_until_a_source_is_opened() {
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

    #[test]
    fn follower_applies_host_transport_and_seeks_on_large_drift() {
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

    #[test]
    fn transport_state_snapshots_engine() {
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
